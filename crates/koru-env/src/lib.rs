// SPDX-License-Identifier: MPL-2.0

//! **Every environment variable Koru reads, declared once.**
//!
//! The same contract as `koru-keys`' chord registry: a table that merely
//! *described* the flags would be one more copy of the truth, so the readers
//! go through this crate — [`switch`] is the one truthiness rule, the name
//! constants in [`name`] make a typo a compile error, and the tests walk the
//! real sources so a flag read anywhere without a row here fails the build.
//! `reference/ENV.md` is generated from [`markdown`], never hand-edited, and
//! `koru.bat envs` / `setup-envs` render and walk the same table.
//!
//! **What belongs here:** Koru's own knobs — runtime switches, value-typed
//! options, the compile-time release stamps, and the variables only the
//! `koru.bat`/`koru.sh` runners read. **What does not:** the platform's
//! variables (`LOCALAPPDATA`, `LANG`, `XDG_*`) — those are where the OS keeps
//! its answers, not switches a person sets for Koru.
//!
//! ## A `.env`, for a session
//!
//! [`load_dotenv`] reads `.env` from the working directory once at startup —
//! today only `koru-api-gateway` calls it. A variable already set in the
//! process wins over the file, and **a key this registry does not declare is
//! refused by name**, because a typo that is silently skipped leaves a feature
//! switched off and nothing to read about it.
//!
//! ## The one truthiness rule
//!
//! Four incompatible rules used to coexist (presence-only, on-unless-`0`,
//! a denylist, a strict allowlist), which made `set KORU_SEARCH_DEBUG=0`
//! *enable* logging — TECHDEBT 29, now closed by [`switch`]: a switch is on
//! when set to anything except `""`, `0`, `false`, `no`, `off` (trimmed,
//! case-insensitive). `set X=1` keeps working everywhere; `set X=0` finally
//! means off everywhere.

/// Whether a variable is an on/off switch or carries a value.
///
/// `setup-envs` walks only the switches; the listing shows both.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Kind {
    Switch,
    /// A path, list, number or command template — see the entry's `values`.
    Value,
}

/// When and by what a variable is read.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Scope {
    /// Read by a running binary; the ones a person sets in a session.
    Runtime,
    /// Baked in with `option_env!` at compile time — set by the release
    /// packager (or a build script), never by a person at run time.
    Compile,
    /// Read only by `koru.bat` / `koru.sh` / `scripts/`; Rust never sees it.
    Script,
}

impl Scope {
    pub fn heading(self) -> &'static str {
        match self {
            Scope::Runtime => "Runtime",
            Scope::Compile => "Compile time",
            Scope::Script => "Scripts only",
        }
    }
}

/// One variable: what it is called, what it does, what it accepts.
pub struct EnvVar {
    pub name: &'static str,
    /// One sentence, present tense, for the listing and the docs.
    pub purpose: &'static str,
    /// What the variable accepts — "on/off", "a path", "`;`-separated URLs".
    pub values: &'static str,
    /// The binary or workflow that changes behaviour.
    pub affects: &'static str,
    pub kind: Kind,
    pub scope: Scope,
    /// Whether the *value* is a credential.
    ///
    /// The listing exists so a person can see what is set, and for every other
    /// variable the value is the interesting half. For a credential it is the
    /// one thing that must not be rendered: `koru.bat envs` is what someone
    /// runs while a thing is misbehaving, and its output is what they paste
    /// into an issue. [`table`] prints `[currently set, hidden]` instead;
    /// [`markdown`] is unchanged, because `reference/ENV.md`
    /// describes variables and never reads one.
    pub secret: bool,
}

pub const fn switch_var(
    name: &'static str,
    purpose: &'static str,
    affects: &'static str,
) -> EnvVar {
    EnvVar {
        name,
        purpose,
        values: "on/off",
        affects,
        kind: Kind::Switch,
        scope: Scope::Runtime,
        secret: false,
    }
}

pub const fn value_var(
    name: &'static str,
    values: &'static str,
    purpose: &'static str,
    affects: &'static str,
) -> EnvVar {
    EnvVar {
        name,
        purpose,
        values,
        affects,
        kind: Kind::Value,
        scope: Scope::Runtime,
        secret: false,
    }
}

/// A value variable whose content is a credential — see [`EnvVar::secret`].
pub const fn secret_var(
    name: &'static str,
    values: &'static str,
    purpose: &'static str,
    affects: &'static str,
) -> EnvVar {
    EnvVar {
        name,
        purpose,
        values,
        affects,
        kind: Kind::Value,
        scope: Scope::Runtime,
        secret: true,
    }
}

pub const fn compile_var(name: &'static str, purpose: &'static str) -> EnvVar {
    EnvVar {
        name,
        purpose,
        values: "set by the packager or build script",
        affects: "release builds",
        kind: Kind::Value,
        scope: Scope::Compile,
        secret: false,
    }
}

pub const fn script_var(
    name: &'static str,
    values: &'static str,
    purpose: &'static str,
    affects: &'static str,
) -> EnvVar {
    EnvVar {
        name,
        purpose,
        values,
        affects,
        kind: Kind::Value,
        scope: Scope::Script,
        secret: false,
    }
}

/// What [`load_file`] did, so a caller can say it in one line.
///
/// **The names, never the values.** A `.env` is where the secrets are, and a
/// boot line naming what it loaded is exactly the line somebody pastes into an
/// issue.
pub struct Loaded {
    pub path: std::path::PathBuf,
    pub names: Vec<&'static str>,
}

#[derive(Debug)]
pub enum EnvFileError {
    Unreadable(String),
    /// A line that is neither blank, a comment, nor `KEY=VALUE`.
    Malformed {
        line: usize,
    },
    /// A key this registry does not declare. Refused rather than ignored --
    /// see [`load_file`].
    Unknown {
        line: usize,
        key: String,
    },
}

impl std::fmt::Display for EnvFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvFileError::Unreadable(message) => write!(f, "{message}"),
            EnvFileError::Malformed { line } => {
                write!(f, "line {line} is not blank, a comment, or KEY=VALUE")
            }
            EnvFileError::Unknown { line, key } => write!(
                f,
                "line {line}: `{key}` is not a Koru environment variable (see reference/ENV.md)"
            ),
        }
    }
}

impl std::error::Error for EnvFileError {}

/// Read `KEY=VALUE` lines from a file into this process's environment.
///
/// `#` comments, blank lines, and an optional pair of surrounding quotes.
/// Two rules carry the whole value of it:
///
/// **A variable already set in the process always wins.** A container's `-e`,
/// or a `set` in the shell you launched from, beats the file — so there is one
/// precedence rule and nothing has to remember it.
///
/// **An unrecognised key is an error, not a warning.** The same doctrine as
/// `koru_workspace_host::cli::parse`: a token we do not understand is not run. A typo
/// in a `.env` must fail at boot with the key named, rather than leave a sink
/// quietly switched off and a person staring at an empty dashboard.
///
/// # Safety
///
/// This calls `std::env::set_var`, which is `unsafe` in edition 2024 because
/// another thread reading the environment concurrently is a data race. **Call
/// it once, from `main`, before anything is spawned** -- which is the only way
/// it is called here.
pub fn load_file(path: &std::path::Path) -> Result<Loaded, EnvFileError> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| EnvFileError::Unreadable(format!("{}: {error}", path.display())))?;
    let mut names = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed).trim();
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(EnvFileError::Malformed { line });
        };
        let key = key.trim();
        let declared =
            VARS.iter()
                .find(|var| var.name == key)
                .ok_or_else(|| EnvFileError::Unknown {
                    line,
                    key: key.to_string(),
                })?;
        let value = unquote(value.trim());
        // Already set wins, and it is not an error to say so twice.
        if std::env::var_os(declared.name).is_some() {
            continue;
        }
        // SAFETY: the contract above -- once, from `main`, before any thread.
        unsafe { std::env::set_var(declared.name, value) };
        names.push(declared.name);
    }
    Ok(Loaded {
        path: path.to_path_buf(),
        names,
    })
}

/// One pair of matching quotes, and nothing cleverer: a `.env` is not a shell,
/// so there is no escaping, no interpolation and no continuation to get wrong.
fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    match (bytes.first(), bytes.last()) {
        (Some(b'"'), Some(b'"')) | (Some(b'\''), Some(b'\'')) if value.len() >= 2 => {
            &value[1..value.len() - 1]
        }
        _ => value,
    }
}

/// Load `.env` from the working directory when there is one.
///
/// **A development convenience, deliberately narrow.** It reads one fixed name
/// from the directory the process was launched in, does nothing when the file
/// is absent, and refuses to start on a key it does not know. A shipped build
/// never has one beside it; `koru.bat run-all` launches from the worktree root,
/// which is where a developer's is.
///
/// Returns the names it set so the caller can log them. See [`load_file`] for
/// the safety contract -- call this once, from `main`.
pub fn load_dotenv() -> Result<Option<Loaded>, EnvFileError> {
    let path = std::path::Path::new(".env");
    if !path.is_file() {
        return Ok(None);
    }
    load_file(path).map(Some)
}

/// The table. Grouped by scope; the diagnostic switches lead because they
/// are what a person reaches for while something is misbehaving.
pub const VARS: &[EnvVar] = &[
    // ------------------------------------------------- diagnostics (switches)
    switch_var(
        name::KORU_INPUT_DEBUG,
        "Logs every OS input event through the dispatch stages before any handler sees it; typed text is redacted, lengths and control code points are kept.",
        "koru.exe",
    ),
    value_var(
        name::KORU_WORKER_OWNS_GROUP,
        "set to 1 by the Agent's supervisor; never set by hand",
        "Marks a unix Agent worker that leads its own process group, so on parent loss it may kill that group and take its descendants with it — the stand-in for Windows' Job Object (M24). Only a process that carries this *and* leads its group ever does it, so nothing else calling the same watcher can kill its own group.",
        "koru-agent workers on macOS and Linux (sessions, gateway, delegate)",
    ),
    switch_var(
        name::KORU_RECORDING_DEBUG,
        "Session-recording lifecycle (internal builds): start/stop, window geometry, frame rate and derived bitrate, telemetry counts. Coordinates and action kinds may appear; never text, titles or names. Failures log unconditionally without it.",
        "koru.exe (internal session recording)",
    ),
    switch_var(
        name::KORU_SEARCH_DEBUG,
        "Search routing and result-open diagnostics, to stderr and a capped per-process file in %TEMP%.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_GOOGLE_DEBUG,
        "Google Docs import OAuth, Picker, keychain and export stages — ids, counts and error codes, never a token or document text.",
        "koru.exe (Google Docs import)",
    ),
    value_var(
        name::KORU_GOOGLE_DOCS_CLIENT_ID,
        "a Google OAuth client id of type Desktop app (…apps.googleusercontent.com)",
        "The Google OAuth client the Google Docs importer signs in with. Baked into a release build from .env.production; with it or its secret unset the importer is shown disabled with a reason. Supply your own registration for a community build (CONTRIBUTING.md).",
        "koru.exe (Google Docs import)",
    ),
    value_var(
        name::KORU_GOOGLE_DOCS_CLIENT_SECRET,
        "the same Desktop app client's secret (GOCSPX-…)",
        "The Google Docs importer's client secret. Google's token endpoint asks for it, but for an installed app it is not confidential: it ships in the binary and PKCE is what authenticates the exchange. Baked into a release build from .env.production.",
        "koru.exe (Google Docs import)",
    ),
    switch_var(
        name::KORU_IMPORT_DEBUG,
        "Evernote import diagnostics — parser pass, structural scope, stream offset, note ordinals, warning codes and resource digests only; never paths, titles, note text or encrypted content.",
        "koru.exe (Evernote import)",
    ),
    switch_var(
        name::KORU_IPC_DEBUG,
        "Endpoint, handshake and every IPC frame, pid-prefixed so two processes' transcripts can be told apart.",
        "koru.exe, koru-agent, koru-api-gateway",
    ),
    switch_var(
        name::KORU_SYNC_DEBUG,
        "The device-sync conversation, including ordinary peer connect/disconnect lifecycles; the shell, Agent, and Gateway share this one variable. Rapid reconnect storms remain visible at INFO without it.",
        "koru.exe, koru-agent, koru-api-gateway",
    ),
    switch_var(
        name::KORU_RELAY_DEBUG,
        "Relay server per-connection and ACME diagnostics — client accepts and disconnects, certificate order progress and frame handling at debug; endpoint ids, counts and stages, never relayed content (which is end-to-end encrypted and unreadable to the relay anyway).",
        "koru-relay",
    ),
    switch_var(
        name::KORU_RUN_MCP_DEBUG,
        "One line per call at a delegated run's MCP endpoint (/v1/runs/{run}/mcp) — the tool's name, the code it answered and the elapsed time; never an argument, a path or a title.",
        "koru-agent --worker gateway",
    ),
    switch_var(
        name::KORU_STORE_SESSION_DEBUG,
        "Store bearer-credential renewals — the reason and the new expiry, never the token itself.",
        "koru.exe, koru-agent, koru-mcp",
    ),
    switch_var(
        name::KORU_PEER_OBJECT_LOGS,
        "Adds per-object workspace_* replication stages on top of the sync log.",
        "koru-agent, koru-api-gateway, Android",
    ),
    value_var(
        name::KORU_CLOUD_BLOB_BYTE_ORIGINS,
        "comma-separated absolute origins, loopback or private-range only (http://127.0.0.1:8081, http://192.168.1.20:8081)",
        "Names a local object worker this device may send blob bytes to, in addition to the compiled-in per-region set. **The production allowlist is not configurable**, because a configurable allowlist is one an attacker configures; this adds a local stack only, refuses any public host, and is how the two-device acceptance test reaches a worker on the tester's own machine. An unparseable or non-local value is refused rather than ignored. **Android has no environment to read this from**, so a phone is told the same value at build time through `koruBlobByteOrigins` in `apps/android/.env.local`, applied through the same allowlist with the same refusals.",
        "koru-store-server (the blob transfer engine); a phone uses koruBlobByteOrigins instead",
    ),
    switch_var(
        name::KORU_CLIPBOARD_DEBUG,
        "Clipboard format names, payload sizes and line/block counts — never contents.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_SPELL_DEBUG,
        "Spell-check requests, timings, language tags and issue counts — never a word, sentence, suggestion or dictionary entry.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_UNDO_DEBUG,
        "The undo and paste-history chain; also on whenever KORU_CLIPBOARD_DEBUG is.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_CANVAS_DEBUG,
        "One line per request a canvas tab applies to its document (add, move, width, arrow, delete, text, agent, run, stop, undo) and whether it was refused — object ids, kinds and refusal tokens, never a card's text or a canvas's name.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_NAV_DEBUG,
        "Tab select, Store document swap, catch-up and spot restore — ids, counts and stages, never a title or row text.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_ACCESSIBILITY_DEBUG,
        "The native accessibility bridge per window: whether a provider was installed, when a screen reader attached or detached, semantic trees built, and actions routed or refused with their stable locator. Window and view slot numbers, occurrence locators, roles, counts and stages — never an accessible name, a label, a document title or any text a person wrote.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_ACCESSIBILITY_DUMP,
        "One dump of the exact accessibility tree a window publishes, taken when a screen reader attaches and never on a frame, timer or poll. Occurrence locators, node ids, parent and order, roles, bounds, states, actions, run and selection references, and counts - never a label, a document title, or a line of text. Costs one atomic load when unset.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_ACCESSIBILITY_DUMP_CONTENT,
        "Adds labels and text-run values to KORU_ACCESSIBILITY_DUMP, which must also be set. A local opt-in for debugging a synthetic fixture: the output then contains document text and must never reach a bug report, a log upload or a shared machine.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_TASKS_DEBUG,
        "Task command outcomes and the desktop wrapper's coalesced counters — commands run, activity attempted, activity dropped — and the two store-backed surfaces' waits on the read model: covered (with the interval since the write, in milliseconds, and the questions asked), gave up, unsupported, unpublished, query failed. Closed tokens, counts, intervals and a causation id; never a title, a due date, row text or a version vector. The store's own tasks counters are on /v1/debug/tasks instead, which needs no switch.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_HISTORY_DEBUG,
        "Version compare stages on the history worker — snapshot sizes, block and row counts and per-stage timings (fetch, decode, compare, unify); never document text.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_ATTACH_DEBUG,
        "Attachment slash command, pill context menu and open/save/reveal/restore path — stages and ids, never file bytes or paths beyond a basename.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_THEME_DEBUG,
        "Every system-theme read and apply, to stderr and a file in the temp directory.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_AI_PAYLOAD_DEBUG,
        "Logs one complete provider-neutral AI request JSON record and one terminal response JSON record per attempt, including authored context and provider output; never logs provider credentials. Also exposes the per-window system-prompt override in AI Settings.",
        "koru.exe, koru-ai-chat, koru-ai-server, koru-api-gateway",
    ),
    value_var(
        name::KORU_AI_HOST_ENDPOINT,
        "an https URL ending in /v1/",
        "Hosted Koru AI Chat Completions base URL. Missing this leaves the Koru AI provider unconfigured.",
        "koru-ai-server",
    ),
    secret_var(
        name::KORU_AI_HOST_KEY,
        "a bearer token",
        "Bearer token for the hosted Koru AI endpoint. Hidden in `koru.bat envs`.",
        "koru-ai-server",
    ),
    switch_var(
        name::KORU_AI_PAYLOAD_TEST_CHILD,
        "Test-only: marks the isolated child process used to prove payload-debug records are opt-in and ordinary AI lifecycle logs remain content-free.",
        "koru-ai-server tests",
    ),
    switch_var(
        name::KORU_FRAME_STATS,
        "Where a frame's time went, summarised every 120 frames.",
        "koru.exe and every shell-based binary",
    ),
    switch_var(
        name::KORU_STARTUP_STATS,
        "Startup span timings — where the time before the first frame went; also arms the stall watch.",
        "koru.exe and every shell-based binary",
    ),
    switch_var(
        name::KORU_SESSION_STATS,
        "Fixed-size terminal-session counters and timing histograms, reported only when explicitly sampled.",
        "koru.exe session tabs, the koru-agent sessions worker, koru-api-gateway",
    ),
    switch_var(
        name::KORU_SESSION_DEBUG,
        "Content-free terminal-session lifecycle and state-transition diagnostics through the shared tracing pipeline.",
        "koru.exe session tabs, the koru-agent sessions worker, koru-api-gateway",
    ),
    switch_var(
        name::KORU_DELEGATE_DEBUG,
        "One line per consequential event in the delegate worker — a status transition, a recorded step, a usage fact, a restart verdict, a refusal — and never one per provider event or message delta; run, attempt and turn ids, step kinds and counts, never an objective, a path, a prompt or a provider payload.",
        "the koru-agent delegate worker",
    ),
    value_var(
        name::KORU_DELEGATE_MCP_ENDPOINT,
        "an http:// or https:// URL",
        "Overrides the base URL a delegated run's MCP endpoint is derived from (`<base>/v1/runs/{run}/mcp`); unset, the delegate worker uses the gateway it discovered. A stub until the run authority (P1.11) hosts the endpoint.",
        "the koru-agent delegate worker",
    ),
    secret_var(
        name::KORU_RUN_MCP_TOKEN,
        "a run endpoint bearer, set by Koru",
        "Not a flag a person sets. Koru sets it on a delegated provider's own child process, and names it to that provider (Codex: `mcp_servers.koru_run.bearer_token_env_var`), so the run's endpoint token reaches the provider through its environment instead of its command line, which every process on the machine can read. Setting it by hand does nothing: a launch overwrites it.",
        "the provider child a delegated run starts",
    ),
    switch_var(
        name::KORU_LIVE_PROVIDER_SMOKE,
        "Runs the opt-in live provider smoke tests against the real CLIs on PATH; unset, those tests return without doing anything.",
        "cargo test -p koru-session-runtime, on a developer machine with an authenticated provider CLI",
    ),
    switch_var(
        name::KORU_JANK,
        "The stall watch: logs any span over the floor (50 ms unless KORU_JANK_MS raises it). `koru.bat jank` sets it for one run.",
        "koru.exe and every shell-based binary",
    ),
    switch_var(
        name::KORU_STORE_STATS,
        "Where the store's time went — metadata-only spans, reported at shutdown.",
        "koru-api-gateway, koru-store-server",
    ),
    switch_var(
        name::KORU_EVENTS_DEBUG,
        "One line per event per hop — sent, recorded (with latency), derived, delivered — names, ids and timestamps only, never attrs.",
        "koru-api-gateway (events worker), koru.exe",
    ),
    switch_var(
        name::KORU_EVENTS_DISABLED,
        "Silences the event bus: the journal is never opened, appends no-op, and every read answers empty.",
        "koru-api-gateway (events worker)",
    ),
    switch_var(
        name::KORU_METRICS_DEBUG,
        "One line per telemetry delivery attempt — event count, outcome code, HTTP status, duration, and the event names and property keys; never a property value.",
        "koru-api-gateway (events worker)",
    ),
    // ------------------------------------------------- the telemetry upload
    //
    // **Overrides, not requirements.** The hosted metrics service
    // (`docs/architecture/eventbus/README.md`) is the default destination:
    // every build has https://m.koru.do as its destination without any of
    // these set, and sends nothing until the person consents on that device.
    // The binary holds no credential — the ingest route is anonymous. These redirect a build (a local service during development)
    // or silence it.
    switch_var(
        name::KORU_TELEMETRY_ENABLED,
        "Whether the events worker has an upload destination at all. Unset, every channel has one; 0 removes it. It never grants consent: nothing is uploaded until the person opts in on this device (the welcome page, or Settings > System), whatever this says.",
        "koru-api-gateway (events worker)",
    ),
    value_var(
        name::KORU_TELEMETRY_BASE_URL,
        "a base URL",
        "Overrides the built-in upload destination (https://m.koru.do); the /v1/metrics path is appended. Point it at a metrics service of your own during development.",
        "koru-api-gateway (events worker)",
    ),
    value_var(
        name::KORU_TELEMETRY_BATCH_EVENTS,
        "1-500 (default 250)",
        "How many events one outbound upload carries.",
        "koru-api-gateway (events worker)",
    ),
    value_var(
        name::KORU_TELEMETRY_TIMEOUT_MS,
        "milliseconds (default 15000)",
        "How long one outbound upload may take before it counts as a timeout and is retried.",
        "koru-api-gateway (events worker)",
    ),
    // ------------------------------------------------- product feature flags
    // Read only by crates/koru-features, baked in at compile time and
    // overridable at run time — see guide/contributing/FEATURE-FLAGS.md.
    switch_var(
        name::KORU_ENABLE_CLOUD_SYNC,
        "Shows Koru Cloud: account sign-in (email, Google, Apple), the Koru Cloud page and its pointers. Off by default; a release bakes it on by setting it for koru-release, and a set value overrides the baked one at run time.",
        "koru.exe, koru-agent (product surfaces)",
    ),
    switch_var(
        name::KORU_ENABLE_AGENTS,
        "Shows agent sessions as tabs (shell sessions are always shown). On by default; a release bakes it off by setting it to 0 for koru-release, and a set value overrides the baked one at run time.",
        "koru.exe, koru-agent (product surfaces)",
    ),
    switch_var(
        name::KORU_ENABLE_AGENT_VIEWS,
        "Shows Explore's Agents and Artifacts views, which also need KORU_ENABLE_AGENTS on. Off by default; a release bakes it on by setting it for koru-release, and a set value overrides the baked one at run time.",
        "koru.exe (product surfaces)",
    ),
    switch_var(
        name::KORU_ENABLE_TASKS,
        "Shows the Tasks view and its entry points (`/due` still creates a task). Off by default; a release bakes it on by setting it for koru-release, and a set value overrides the baked one at run time.",
        "koru.exe (product surfaces)",
    ),
    switch_var(
        name::KORU_ENABLE_AI,
        "Shows Koru AI chat and Delegate. Off by default; a release bakes it on by setting it for koru-release, and a set value overrides the baked one at run time.",
        "koru.exe, koru-agent (product surfaces)",
    ),
    switch_var(
        name::KORU_ENABLE_REMINDERS,
        "Shows the reminders UI (the reminder worker runs either way). Off by default; a release bakes it on by setting it for koru-release, and a set value overrides the baked one at run time.",
        "koru.exe, koru-agent (product surfaces)",
    ),
    switch_var(
        name::KORU_ENABLE_CANVAS,
        "Shows canvases: the Canvases view (Alt+7), the `+` menu's Canvases flyout and canvas tabs. Off by default; a release bakes it on by setting it for koru-release, and a set value overrides the baked one at run time.",
        "koru.exe (product surfaces)",
    ),
    switch_var(
        name::KORU_ENABLE_SOCIALS,
        "Shows Socials: the Socials view (Alt+8), New social post and the Socials tab, and starts the Agent's Socials publisher (off, nothing publishes; scheduled posts wait). Off by default; a release bakes it on by setting it for koru-release, and a set value overrides the baked one at run time.",
        "koru.exe (product surfaces), koru-agent.exe (the Socials publisher)",
    ),
    // -------------------------------------------- behaviour / test switches
    switch_var(
        name::KORU_STORE_NO_DIALOG,
        "Suppresses the fatal message box when a store cannot open its databases — set by every store integration test.",
        "koru-api-gateway, koru-store-server, tests",
    ),
    switch_var(
        name::KORU_AGENT_TEST_HOLD_SHELL_LAUNCH,
        "Test-only: keeps a rooted Agent's successful shell launch pending until the lifecycle fixture sends the matching Hello.",
        "koru-agent lifecycle tests",
    ),
    value_var(
        name::KORU_PUBLIC_API_ORIGIN,
        "an HTTPS origin, or literal loopback HTTP for local development",
        "Sets the canonical public API origin used for fixed provider callbacks and native Agent account calls. Baked into a release build from .env.production, because an installed build has no environment to read it from. Without it the build has no Koru Cloud: sign-in and Cloud sync are off, and the app says Koru Cloud isn't set up in this build (koru_env::cloud_configured).",
        "koru-agent",
    ),
    value_var(
        name::KORU_PUBLIC_API_ISSUER,
        "the public API's HTTPS origin, or literal loopback HTTP for local development",
        "Sets and verifies the account access-token issuer. Baked into a release build from .env.production.",
        "koru-agent",
    ),
    value_var(
        name::KORU_PUBLIC_API_CREDENTIAL_ROLLOUT,
        "optional strict JSON regional credential policy; absent preserves legacy issuance",
        "Configures exact regional issuers, US residency, issuance stage, cutover time and purpose-specific legacy key ids. Contains no secrets.",
        "koru-agent",
    ),
    // ------------------------------------- rehearsal, measurement, deployment
    switch_var(
        name::KORU_REHEARSAL_FAIL_UPDATE_HEALTH,
        "Release rehearsal only: makes a token-bearing Agent under an explicit root exit before publishing update health, forcing the real updater rollback path.",
        "koru-agent rooted update rehearsal",
    ),
    switch_var(
        name::KORU_NO_GPU_WARMUP,
        "A/B measurement switch: puts GPU adapter, device and renderer creation (the shader compile) back on the main thread, at the first window. Nothing in the product reads it; CI sets it for the tests, because a runner without a GPU compiles on the software rasterizer.",
        "koru.exe, koru-agent",
    ),
    switch_var(
        name::KORU_IROH_ISOLATED,
        "Deployment-only: isolated iroh networking with no shared relays or discovery.",
        "koru-network-iroh (agent, store server, mobile host)",
    ),
    // The one voice switch. It sits with the behaviour switches rather than
    // with the diagnostics above because it turns a feature off; the rest of
    // the voice family is value-typed and further down. There is deliberately
    // no KORU_VOICE_DEBUG — LOGGING.md reserves a feature-specific switch for
    // a diagnostic that changes behaviour, destination or aggregation, and the
    // worker's stage trace is a target's debug level, which RUST_LOG already
    // turns up (see apps/koru-voice/src/log.rs and QUESTIONS Q-V40).
    switch_var(
        name::KORU_VOICE_DISABLED,
        "Turns dictation entirely off: no voice worker starts, no dictation chord is bound, and every voice surface says it is unavailable. Nothing about a dictation is logged because none runs — and no path this switch guards has ever logged audio, a transcript, or a capture device's name.",
        "koru.exe, koru-agent, koru-voice-worker",
    ),
    switch_var(
        name::KORU_UPDATE_SHORTCUTS,
        "Rewrites reference/SHORTCUTS.md during `cargo test -p koru-keys` instead of asserting it is current.",
        "cargo test",
    ),
    switch_var(
        name::KORU_UPDATE_ENV,
        "Rewrites reference/ENV.md during `cargo test -p koru-env` instead of asserting it is current.",
        "cargo test",
    ),
    switch_var(
        name::KORU_UPDATE_EVENTS,
        "Rewrites reference/EVENTS.md and the serialization vectors during `cargo test -p koru-events` instead of asserting they are current.",
        "cargo test",
    ),
    switch_var(
        name::KORU_UPDATE_WORKSPACE_CRYPTO_VECTORS,
        "Rewrites the workspace-crypto golden vectors under contracts/vectors during `cargo test -p koru-workspace-crypto` instead of asserting they are current.",
        "cargo test",
    ),
    switch_var(
        name::KORU_UPDATE_VOICE_VECTORS,
        "Rewrites the voice wire vectors in crates/koru-voice/tests/vectors during `cargo test -p koru-voice` instead of asserting they are current.",
        "cargo test",
    ),
    switch_var(
        name::KORU_UPDATE_SESSION_SCHEMA,
        "Rewrites the generated terminal-session protocol schema during its focused snapshot test instead of asserting it is current.",
        "cargo test -p koru-session-protocol",
    ),
    switch_var(
        name::KORU_UPDATE_PUBLIC_API_SCHEMA,
        "Rewrites the generated account wire schema during its focused snapshot test instead of asserting it is current.",
        "cargo test -p koru-account-wire",
    ),
    // -------------------------------------------------- value-typed options
    value_var(
        name::KORU_JANK_MS,
        "milliseconds",
        "Raises the stall watch's floor; a non-number falls back to 50 ms rather than disabling the watch.",
        "koru.exe and every shell-based binary",
    ),
    value_var(
        name::KORU_JANK_LOG,
        "a file path",
        "Appends stall lines to a shareable file in addition to the console; `koru.bat jank` points it at koru-jank.log.",
        "koru.exe and every shell-based binary",
    ),
    value_var(
        name::KORU_ASSETS,
        "a directory path",
        "An assets directory searched before the exe-relative and source-tree roots; fonts are read from `<KORU_ASSETS>/fonts/`.",
        "anything that draws text",
    ),
    value_var(
        name::KORU_IROH_RELAYS,
        "`;`-separated relay URLs",
        "Overrides the compiled Koru relay list with custom relays. Empty entries are dropped, and a value with none left counts as unset.",
        "koru-network-iroh (agent, store server, mobile host)",
    ),
    value_var(
        name::KORU_SEARCH_MATCH,
        "internal",
        "One-shot match text handed to a child search window so authored text never appears in argv. Never set by hand.",
        "koru.exe",
    ),
    value_var(
        name::KORU_FEEDBACK_URL,
        "a URL",
        "Where in-app feedback is POSTed; defaults to https://www.koru.do/api/feedback. Point it at http://localhost:3000/api/feedback to hit a local marketing site.",
        "koru.exe",
    ),
    value_var(
        name::KORU_SUPPORT_URL,
        "an origin",
        "Desktop website origin for support and Cloud subscription links (scheme and host, no path), overriding https://www.koru.do; http://localhost:3000 uses a local marketing site. The app appends the locale and /support/desktop, /pricing or /dashboard.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_SUPPORT_DEBUG,
        "The support chat's routine lines -- boot requested, ready, the close-dialog choice -- to stderr. Never message text.",
        "koru.exe",
    ),
    switch_var(
        name::KORU_WEBVIEW_DEBUG,
        "The embedded web surface's stages and navigation decisions, to stderr as stage, decision and host -- never a URL or a message body. On a dev build also opens the engine's devtools.",
        "koru.exe",
    ),
    value_var(
        name::KORU_WEBVIEW_FAULT,
        "create",
        "Dev builds only: make the embedded web surface fail to create, so the support window's error state can be demonstrated without breaking the machine.",
        "koru.exe",
    ),
    value_var(
        name::KORU_NOTES_PREVIEW,
        "a file path",
        "Test-only: writes a rendered release-notes HTML page there for eyeballing.",
        "cargo test -p koru-release",
    ),
    value_var(
        name::KORU_RELEASES_PREVIEW,
        "a file path",
        "Test-only: writes the rendered release index page there for eyeballing.",
        "cargo test -p koru-release",
    ),
    value_var(
        name::KORU_NOTES_COMMAND,
        "`codex`, `kimi`, or a command template with `{channel}`",
        "The generator dist-cloud offers for a missing release descriptor. Unset, there is no default: the prompt offers codex or kimi, or stops so the descriptor can be written by hand.",
        "koru-release",
    ),
    value_var(
        name::KORU_SIGNTOOL_COMMAND,
        "a command template with `{file}`",
        "Authenticode signing hook for each packaged binary and archive; unset means unsigned.",
        "koru-release",
    ),
    value_var(
        name::KORU_WINDOWS_PACKAGE_PUBLISHER,
        "an X.509 subject name",
        "Publisher identity for the signed sparse Windows notification package; must match the certificate used by KORU_SIGNTOOL_COMMAND.",
        "koru-release and koru-agent release builds",
    ),
    value_var(
        name::KORU_MAC_SIGN_COMMAND,
        "a command template with `{file}`",
        "macOS codesign hook, run per helper and once for the bundle and the dmg; unset means ad hoc.",
        "koru-release",
    ),
    value_var(
        name::KORU_MAC_NOTARIZE_COMMAND,
        "a command template with `{file}`",
        "macOS notarization hook handed a zip of the bundle; unset skips notarization and the release page says so.",
        "koru-release",
    ),
    secret_var(
        name::CLOUDFLARE_API_TOKEN,
        "an API token",
        "Cloudflare credentials for `cloud-setup` and `dist-cloud` — R2, DNS and cache operations.",
        "koru-release",
    ),
    value_var(
        name::CLOUDFLARE_R2_ACCESS_KEY_ID,
        "an R2 S3 Access Key ID",
        "Cloudflare R2 S3 identity for multipart release objects; set with CLOUDFLARE_R2_SECRET_ACCESS_KEY.",
        "koru-release",
    ),
    secret_var(
        name::CLOUDFLARE_R2_SECRET_ACCESS_KEY,
        "an R2 S3 Secret Access Key",
        "Cloudflare R2 S3 secret for multipart release objects; set with CLOUDFLARE_R2_ACCESS_KEY_ID.",
        "koru-release",
    ),
    value_var(
        name::RUST_LOG,
        "a tracing filter directive",
        "Overrides a binary's default tracing filter — e.g. `koru_replication=debug`.",
        "koru, koru-ai-chat, koru-api-gateway, koru-store-server, koru-agent, koru-mcp, koru-relay, koru-voice-worker",
    ),
    value_var(
        name::WGPU_BACKEND,
        "dx12, vulkan, gl (comma-separated to allow several)",
        "Diagnostic override for the GPU backend, read inside wgpu itself; wins over the Settings tab's choice in renderer.conf. A backend with no usable adapter falls back to the default and logs, rather than failing the launch.",
        "koru.exe, koru-agent",
    ),
    value_var(
        name::KORU_INTERNAL_STORE_READY,
        "an inherited OS handle or file descriptor",
        "Internal test-only readiness channel for the standalone Store harness; never set by hand.",
        "koru-store-server tests",
    ),
    value_var(
        name::KORU_INTERNAL_SETTINGS_WRITE_FAULT,
        "global-dictation-enable, global-dictation-disable, or start-on-login-disable",
        "Internal-build-only one-shot hold of one named confirmed settings reply after the authority receives and applies it; ignored by released builds.",
        "koru-api-gateway (internal builds only)",
    ),
    value_var(
        name::KORU_VOICE_MODELS,
        "a directory path",
        "The speech-model cache, overriding both `--root` and the channel-independent default. It exists so a rooted test reuses one download rather than paying for isolation in gigabytes, and it deliberately points outside the profile root it overrides.",
        "koru-voice-worker, and anything that lists or installs a speech model",
    ),
    value_var(
        name::KORU_VOICE_CATALOG,
        "a file path, or an https URL",
        "Where the speech-model catalog is read from, overriding the built-in `https://updates.koru.do/voice/catalog-v1.json`. A value naming a file that exists is read as a path and anything else is parsed as a URL, which must be HTTPS - so an air-gapped install and a fixture server are the same mechanism rather than two. It changes where an *offer* comes from and never what is trusted: every entry still passes the model trust policy before a byte is fetched - the pinned model-signing key, or, while the MVP key set is empty, the named digest-only arm that no stable build accepts (ADR-250's amendment).",
        "koru.exe, koru-agent, and anything that offers a speech model",
    ),
    value_var(
        name::KORU_VOICE_WHISPER_MODEL,
        "a directory path",
        "Where the provisioned Compact (whisper) model directory lives, for the worker's recorded-speech acceptance test. Test-only: unset, the test skips rather than fails, because the weights are a deliberate download and never a fixture.",
        "koru-voice-worker tests",
    ),
    value_var(
        name::KORU_VOICE_BENCH_CORPUS,
        "a directory path",
        "Where the engine benchmark's decision corpus lives, when `--corpus` is not given. The corpus is downloaded and never committed - it is gigabytes of licensed speech - so there is deliberately no default and no search path: a benchmark that silently found the wrong corpus would answer the engine question with the wrong numbers.",
        "koru-voice-bench",
    ),
    value_var(
        name::KORU_VOICE_BENCH_MODELS,
        "a directory path",
        "Where the engine benchmark's provisioned model directories live, when `--models` is not given. Each chunk size is its own export and therefore its own directory, so this names a tree rather than a file.",
        "koru-voice-bench",
    ),
    value_var(
        name::KORU_VOICE_SHERPA_MODEL,
        "a directory path",
        "One provisioned sherpa model directory, which the voice worker's manual dictating suite loads for its end-to-end proof that audio becomes text. There is deliberately no default and no search path: the weights are half a gigabyte and gitignored, so a test that silently found nothing would pass on a fresh clone while proving nothing. Unset, that suite reports why it did not run and passes; it is never a checked-in test that depends on a file most machines do not have.",
        "koru-voice-worker, under `cargo test -- dictating --nocapture`",
    ),
    value_var(
        name::KORU_VOICE_IDLE_UNLOAD,
        "whole seconds; 0 never unloads",
        "How long the voice worker keeps a speech model in memory with no dictation running. Several hundred megabytes are only genuinely released by letting go, and five minutes is chosen so a person who dictates, reads, and dictates again does not pay the load twice.",
        "koru-voice-worker",
    ),
    value_var(
        name::KORU_VOICE_IDLE_EXIT,
        "whole seconds; 0 never exits",
        "How long the voice worker itself stays running with no dictation. Ending the process is how the memory is released rather than nominally released, and Agent starts a new worker on the next dictation.",
        "koru-voice-worker",
    ),
    value_var(
        name::KORU_DICTIONARIES,
        "a directory path",
        "Where the spell-check dictionaries live, overriding both `--root` and the local-data default. One directory per language tag (`en-US/dictionary.aff` + `dictionary.dic`), written by `scripts/setup/fetch-dictionaries`. The corpus and benchmark tests read *only* this variable and skip when it is unset - they never perform the default-directory resolution, so a machine with dictionaries installed runs the same suite CI does.",
        "koru-spell",
    ),
    // -------------------------------------------------- compile-time stamps
    compile_var(
        name::KORU_VERSION,
        "The version stamped into the binaries; falls back to the crate version in a dev build.",
    ),
    compile_var(
        name::KORU_BUILD_NUMBER,
        "The monotonic build number; 0 means \"not a release\" and disables updates.",
    ),
    compile_var(
        name::KORU_RELEASE_CHANNEL,
        "The data channel — store root, agent root, every IPC endpoint name. Compile-time on purpose: a channel is chosen by downloading it (ADR-113).",
    ),
    compile_var(name::KORU_COMMIT, "The commit the release was built from."),
    compile_var(
        name::KORU_RELEASED_AT,
        "The release timestamp; empty reads as \"built\", not \"released\".",
    ),
    compile_var(
        name::KORU_UPDATE_PUBLIC_KEY,
        "The minisign key updates are verified against; empty disables updates.",
    ),
    compile_var(
        name::KORU_UPDATER_SHA256,
        "The expected updater digest; empty disables updates.",
    ),
    compile_var(
        name::KORU_BRANCH,
        "The git branch, stamped by the build script itself — emitted, never set by a person.",
    ),
    compile_var(
        name::KORU_WINDOWS_PACKAGE_VERSION,
        "The four-part sparse-package version derived by the Agent build script from KORU_VERSION and KORU_BUILD_NUMBER; emitted, never set by a person.",
    ),
    compile_var(
        name::KORU_UPDATE_BASE_URL,
        "Where update manifests are fetched from; defaults to https://updates.koru.do/.",
    ),
    // ------------------------------------------------------- scripts only
    script_var(
        name::KORU_ROOT,
        "a directory path",
        "koru.bat expands it to `--root` so a whole private stack (store, agent, app) runs beside your real one.",
        "koru.bat, scripts/",
    ),
    script_var(
        name::KORU_FONTS_DIR,
        "a directory path",
        "An extra directory fetch-fonts copies from before reaching for the network.",
        "scripts/setup/fetch-fonts",
    ),
    script_var(
        name::KORU_JAVA_HOME,
        "a JDK path",
        "The JDK the Android runner uses, when discovery should not choose.",
        "apps/android/koru.bat",
    ),
    script_var(
        name::KORU_ANDROID_HOME,
        "an SDK path",
        "The Android SDK the Android runner uses.",
        "apps/android/koru.bat",
    ),
    script_var(
        name::KORU_DEVICE,
        "a device serial or alias",
        "Which device or emulator the mobile runners target.",
        "mobile runners",
    ),
    script_var(
        name::KORU_CONFIGURATION,
        "Debug or Release",
        "The Xcode configuration the iOS runner builds.",
        "apps/ios/koru.sh",
    ),
    script_var(
        name::KORU_DEVICE_KIND,
        "simulator or device",
        "Whether the iOS runner targets a simulator or hardware.",
        "apps/ios/koru.sh",
    ),
    script_var(
        name::KORU_DEVELOPMENT_TEAM,
        "an Apple team id",
        "The signing team for iOS device builds.",
        "apps/ios/koru.sh",
    ),
];

/// The names, as constants, so a reader cannot typo one — the same reason
/// `koru-keys` has `ShortcutId` rather than strings.
pub mod name {
    pub const KORU_ENABLE_CLOUD_SYNC: &str = "KORU_ENABLE_CLOUD_SYNC";
    pub const KORU_ENABLE_AGENTS: &str = "KORU_ENABLE_AGENTS";
    pub const KORU_ENABLE_AGENT_VIEWS: &str = "KORU_ENABLE_AGENT_VIEWS";
    pub const KORU_ENABLE_TASKS: &str = "KORU_ENABLE_TASKS";
    pub const KORU_ENABLE_AI: &str = "KORU_ENABLE_AI";
    pub const KORU_ENABLE_REMINDERS: &str = "KORU_ENABLE_REMINDERS";
    pub const KORU_ENABLE_CANVAS: &str = "KORU_ENABLE_CANVAS";
    pub const KORU_ENABLE_SOCIALS: &str = "KORU_ENABLE_SOCIALS";
    pub const KORU_INPUT_DEBUG: &str = "KORU_INPUT_DEBUG";
    pub const KORU_RECORDING_DEBUG: &str = "KORU_RECORDING_DEBUG";
    pub const KORU_WORKER_OWNS_GROUP: &str = "KORU_WORKER_OWNS_GROUP";
    pub const KORU_SEARCH_DEBUG: &str = "KORU_SEARCH_DEBUG";
    pub const KORU_GOOGLE_DEBUG: &str = "KORU_GOOGLE_DEBUG";
    pub const KORU_GOOGLE_DOCS_CLIENT_ID: &str = "KORU_GOOGLE_DOCS_CLIENT_ID";
    pub const KORU_GOOGLE_DOCS_CLIENT_SECRET: &str = "KORU_GOOGLE_DOCS_CLIENT_SECRET";
    pub const KORU_IMPORT_DEBUG: &str = "KORU_IMPORT_DEBUG";
    pub const KORU_IPC_DEBUG: &str = "KORU_IPC_DEBUG";
    pub const KORU_RUN_MCP_DEBUG: &str = "KORU_RUN_MCP_DEBUG";
    pub const KORU_SYNC_DEBUG: &str = "KORU_SYNC_DEBUG";
    pub const KORU_RELAY_DEBUG: &str = "KORU_RELAY_DEBUG";
    pub const KORU_STORE_SESSION_DEBUG: &str = "KORU_STORE_SESSION_DEBUG";
    pub const KORU_PEER_OBJECT_LOGS: &str = "KORU_PEER_OBJECT_LOGS";
    pub const KORU_CLOUD_BLOB_BYTE_ORIGINS: &str = "KORU_CLOUD_BLOB_BYTE_ORIGINS";
    pub const KORU_CLIPBOARD_DEBUG: &str = "KORU_CLIPBOARD_DEBUG";
    pub const KORU_UNDO_DEBUG: &str = "KORU_UNDO_DEBUG";
    pub const KORU_NAV_DEBUG: &str = "KORU_NAV_DEBUG";
    pub const KORU_CANVAS_DEBUG: &str = "KORU_CANVAS_DEBUG";
    pub const KORU_ACCESSIBILITY_DEBUG: &str = "KORU_ACCESSIBILITY_DEBUG";
    pub const KORU_ACCESSIBILITY_DUMP: &str = "KORU_ACCESSIBILITY_DUMP";
    pub const KORU_ACCESSIBILITY_DUMP_CONTENT: &str = "KORU_ACCESSIBILITY_DUMP_CONTENT";
    pub const KORU_TASKS_DEBUG: &str = "KORU_TASKS_DEBUG";
    pub const KORU_HISTORY_DEBUG: &str = "KORU_HISTORY_DEBUG";
    pub const KORU_ATTACH_DEBUG: &str = "KORU_ATTACH_DEBUG";
    pub const KORU_THEME_DEBUG: &str = "KORU_THEME_DEBUG";
    pub const KORU_AI_PAYLOAD_DEBUG: &str = "KORU_AI_PAYLOAD_DEBUG";
    pub const KORU_AI_PAYLOAD_TEST_CHILD: &str = "KORU_AI_PAYLOAD_TEST_CHILD";
    pub const KORU_AI_HOST_ENDPOINT: &str = "KORU_AI_HOST_ENDPOINT";
    pub const KORU_AI_HOST_KEY: &str = "KORU_AI_HOST_KEY";
    pub const KORU_FRAME_STATS: &str = "KORU_FRAME_STATS";
    pub const KORU_STARTUP_STATS: &str = "KORU_STARTUP_STATS";
    pub const KORU_SESSION_STATS: &str = "KORU_SESSION_STATS";
    pub const KORU_SESSION_DEBUG: &str = "KORU_SESSION_DEBUG";
    pub const KORU_DELEGATE_DEBUG: &str = "KORU_DELEGATE_DEBUG";
    pub const KORU_DELEGATE_MCP_ENDPOINT: &str = "KORU_DELEGATE_MCP_ENDPOINT";
    pub const KORU_RUN_MCP_TOKEN: &str = "KORU_RUN_MCP_TOKEN";
    pub const KORU_LIVE_PROVIDER_SMOKE: &str = "KORU_LIVE_PROVIDER_SMOKE";
    pub const KORU_JANK: &str = "KORU_JANK";
    pub const KORU_JANK_MS: &str = "KORU_JANK_MS";
    pub const KORU_JANK_LOG: &str = "KORU_JANK_LOG";
    pub const KORU_STORE_STATS: &str = "KORU_STORE_STATS";
    pub const KORU_EVENTS_DEBUG: &str = "KORU_EVENTS_DEBUG";
    pub const KORU_EVENTS_DISABLED: &str = "KORU_EVENTS_DISABLED";
    pub const KORU_METRICS_DEBUG: &str = "KORU_METRICS_DEBUG";
    pub const KORU_TELEMETRY_ENABLED: &str = "KORU_TELEMETRY_ENABLED";
    pub const KORU_TELEMETRY_BASE_URL: &str = "KORU_TELEMETRY_BASE_URL";
    pub const KORU_TELEMETRY_BATCH_EVENTS: &str = "KORU_TELEMETRY_BATCH_EVENTS";
    pub const KORU_TELEMETRY_TIMEOUT_MS: &str = "KORU_TELEMETRY_TIMEOUT_MS";
    pub const KORU_STORE_NO_DIALOG: &str = "KORU_STORE_NO_DIALOG";
    pub const KORU_AGENT_TEST_HOLD_SHELL_LAUNCH: &str = "KORU_AGENT_TEST_HOLD_SHELL_LAUNCH";
    pub const KORU_PUBLIC_API_ORIGIN: &str = "KORU_PUBLIC_API_ORIGIN";
    pub const KORU_PUBLIC_API_ISSUER: &str = "KORU_PUBLIC_API_ISSUER";
    pub const KORU_PUBLIC_API_CREDENTIAL_ROLLOUT: &str = "KORU_PUBLIC_API_CREDENTIAL_ROLLOUT";
    pub const KORU_REHEARSAL_FAIL_UPDATE_HEALTH: &str = "KORU_REHEARSAL_FAIL_UPDATE_HEALTH";
    pub const KORU_NO_GPU_WARMUP: &str = "KORU_NO_GPU_WARMUP";
    pub const KORU_IROH_ISOLATED: &str = "KORU_IROH_ISOLATED";
    pub const KORU_IROH_RELAYS: &str = "KORU_IROH_RELAYS";
    pub const KORU_UPDATE_SHORTCUTS: &str = "KORU_UPDATE_SHORTCUTS";
    pub const KORU_UPDATE_ENV: &str = "KORU_UPDATE_ENV";
    pub const KORU_UPDATE_EVENTS: &str = "KORU_UPDATE_EVENTS";
    pub const KORU_UPDATE_VOICE_VECTORS: &str = "KORU_UPDATE_VOICE_VECTORS";
    pub const KORU_UPDATE_WORKSPACE_CRYPTO_VECTORS: &str = "KORU_UPDATE_WORKSPACE_CRYPTO_VECTORS";
    pub const KORU_UPDATE_SESSION_SCHEMA: &str = "KORU_UPDATE_SESSION_SCHEMA";
    pub const KORU_UPDATE_PUBLIC_API_SCHEMA: &str = "KORU_UPDATE_PUBLIC_API_SCHEMA";
    pub const KORU_ASSETS: &str = "KORU_ASSETS";
    pub const KORU_SEARCH_MATCH: &str = "KORU_SEARCH_MATCH";
    pub const KORU_NOTES_PREVIEW: &str = "KORU_NOTES_PREVIEW";
    pub const KORU_RELEASES_PREVIEW: &str = "KORU_RELEASES_PREVIEW";
    pub const KORU_NOTES_COMMAND: &str = "KORU_NOTES_COMMAND";
    pub const KORU_SIGNTOOL_COMMAND: &str = "KORU_SIGNTOOL_COMMAND";
    pub const KORU_WINDOWS_PACKAGE_PUBLISHER: &str = "KORU_WINDOWS_PACKAGE_PUBLISHER";
    pub const KORU_WINDOWS_PACKAGE_VERSION: &str = "KORU_WINDOWS_PACKAGE_VERSION";
    pub const KORU_MAC_SIGN_COMMAND: &str = "KORU_MAC_SIGN_COMMAND";
    pub const KORU_MAC_NOTARIZE_COMMAND: &str = "KORU_MAC_NOTARIZE_COMMAND";
    pub const CLOUDFLARE_API_TOKEN: &str = "CLOUDFLARE_API_TOKEN";
    pub const CLOUDFLARE_R2_ACCESS_KEY_ID: &str = "CLOUDFLARE_R2_ACCESS_KEY_ID";
    pub const CLOUDFLARE_R2_SECRET_ACCESS_KEY: &str = "CLOUDFLARE_R2_SECRET_ACCESS_KEY";
    pub const RUST_LOG: &str = "RUST_LOG";
    pub const WGPU_BACKEND: &str = "WGPU_BACKEND";
    pub const KORU_INTERNAL_STORE_READY: &str = "KORU_INTERNAL_STORE_READY";
    pub const KORU_INTERNAL_SETTINGS_WRITE_FAULT: &str = "KORU_INTERNAL_SETTINGS_WRITE_FAULT";
    pub const KORU_VOICE_DISABLED: &str = "KORU_VOICE_DISABLED";
    pub const KORU_VOICE_MODELS: &str = "KORU_VOICE_MODELS";
    pub const KORU_VOICE_CATALOG: &str = "KORU_VOICE_CATALOG";
    pub const KORU_VOICE_WHISPER_MODEL: &str = "KORU_VOICE_WHISPER_MODEL";
    pub const KORU_VOICE_BENCH_CORPUS: &str = "KORU_VOICE_BENCH_CORPUS";
    pub const KORU_VOICE_BENCH_MODELS: &str = "KORU_VOICE_BENCH_MODELS";
    pub const KORU_VOICE_SHERPA_MODEL: &str = "KORU_VOICE_SHERPA_MODEL";
    pub const KORU_VOICE_IDLE_UNLOAD: &str = "KORU_VOICE_IDLE_UNLOAD";
    pub const KORU_DICTIONARIES: &str = "KORU_DICTIONARIES";
    pub const KORU_SPELL_DEBUG: &str = "KORU_SPELL_DEBUG";
    pub const KORU_VOICE_IDLE_EXIT: &str = "KORU_VOICE_IDLE_EXIT";
    pub const KORU_VERSION: &str = "KORU_VERSION";
    pub const KORU_BUILD_NUMBER: &str = "KORU_BUILD_NUMBER";
    pub const KORU_RELEASE_CHANNEL: &str = "KORU_RELEASE_CHANNEL";
    pub const KORU_COMMIT: &str = "KORU_COMMIT";
    pub const KORU_RELEASED_AT: &str = "KORU_RELEASED_AT";
    pub const KORU_UPDATE_PUBLIC_KEY: &str = "KORU_UPDATE_PUBLIC_KEY";
    pub const KORU_UPDATER_SHA256: &str = "KORU_UPDATER_SHA256";
    pub const KORU_BRANCH: &str = "KORU_BRANCH";
    pub const KORU_UPDATE_BASE_URL: &str = "KORU_UPDATE_BASE_URL";
    pub const KORU_FEEDBACK_URL: &str = "KORU_FEEDBACK_URL";
    pub const KORU_SUPPORT_URL: &str = "KORU_SUPPORT_URL";
    pub const KORU_SUPPORT_DEBUG: &str = "KORU_SUPPORT_DEBUG";
    pub const KORU_WEBVIEW_DEBUG: &str = "KORU_WEBVIEW_DEBUG";
    pub const KORU_WEBVIEW_FAULT: &str = "KORU_WEBVIEW_FAULT";
    pub const KORU_ROOT: &str = "KORU_ROOT";
    pub const KORU_FONTS_DIR: &str = "KORU_FONTS_DIR";
    pub const KORU_JAVA_HOME: &str = "KORU_JAVA_HOME";
    pub const KORU_ANDROID_HOME: &str = "KORU_ANDROID_HOME";
    pub const KORU_DEVICE: &str = "KORU_DEVICE";
    pub const KORU_CONFIGURATION: &str = "KORU_CONFIGURATION";
    pub const KORU_DEVICE_KIND: &str = "KORU_DEVICE_KIND";
    pub const KORU_DEVELOPMENT_TEAM: &str = "KORU_DEVELOPMENT_TEAM";
}

/// The entry for a name, when it has one.
pub fn get(name: &str) -> Option<&'static EnvVar> {
    VARS.iter().find(|var| var.name == name)
}

/// **The one truthiness rule.** On when set to anything except `""`, `0`,
/// `false`, `no`, `off` — trimmed, case-insensitive. Unset is off.
///
/// This is the whole of TECHDEBT 29's fix: `set X=1` works the way it always
/// has, and `set X=0` means off for every switch, including the ones that
/// used to be presence-only.
pub fn switch(name: &str) -> bool {
    switch_value(std::env::var(name).ok().as_deref())
}

/// The same rule for a value a caller already has — an injected parser, a
/// `.env` map, a container's supplied environment. `switch` is this function
/// plus `std::env::var`, so a second reader cannot invent a different rule.
pub fn switch_value(value: Option<&str>) -> bool {
    match value {
        Some(value) => {
            let value = value.trim().to_ascii_lowercase();
            !matches!(value.as_str(), "" | "0" | "false" | "no" | "off")
        }
        None => false,
    }
}

/// A value-typed variable, trimmed, `None` when unset or empty.
/// The runtime values a release build carries with it.
///
/// A development launch reads these from `.env.production` or `.env.local`
/// through the shell. An installed build has no shell and no file: the
/// first nightly with Cloud sync had no API origin at all, the account
/// capability never started, and "Sign in with Google" went to an Agent
/// with nothing to start it with. The release tool loads `.env.production`
/// before it builds, and these seven are baked into the binaries with
/// `option_env!`. None of them is a secret: two origins, a public rollout
/// policy, two website URLs, and the Google Docs importer's installed-app
/// OAuth registration — whose "secret" Google itself treats as public for a
/// desktop client, because anything in a binary can be extracted; PKCE is
/// what authenticates. The environment still wins when it is set, so
/// `run-all-local` against a local stack is unchanged.
///
/// **This is configuration, not confidentiality.** Baking the importer's
/// registration from the build environment rather than from a literal is
/// what keeps the official registration out of the public source and out of
/// community builds; a build without it shows the importer disabled.
pub const BAKED: [&str; 7] = [
    name::KORU_PUBLIC_API_ORIGIN,
    name::KORU_PUBLIC_API_ISSUER,
    name::KORU_PUBLIC_API_CREDENTIAL_ROLLOUT,
    name::KORU_SUPPORT_URL,
    name::KORU_FEEDBACK_URL,
    name::KORU_GOOGLE_DOCS_CLIENT_ID,
    name::KORU_GOOGLE_DOCS_CLIENT_SECRET,
];

fn baked(name: &str) -> Option<&'static str> {
    let value = match name {
        name::KORU_PUBLIC_API_ORIGIN => option_env!("KORU_PUBLIC_API_ORIGIN"),
        name::KORU_PUBLIC_API_ISSUER => option_env!("KORU_PUBLIC_API_ISSUER"),
        name::KORU_PUBLIC_API_CREDENTIAL_ROLLOUT => {
            option_env!("KORU_PUBLIC_API_CREDENTIAL_ROLLOUT")
        }
        name::KORU_SUPPORT_URL => option_env!("KORU_SUPPORT_URL"),
        name::KORU_FEEDBACK_URL => option_env!("KORU_FEEDBACK_URL"),
        name::KORU_GOOGLE_DOCS_CLIENT_ID => option_env!("KORU_GOOGLE_DOCS_CLIENT_ID"),
        name::KORU_GOOGLE_DOCS_CLIENT_SECRET => option_env!("KORU_GOOGLE_DOCS_CLIENT_SECRET"),
        _ => None,
    };
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// A runtime value: the process environment first, then what the release
/// baked in for the names in [`BAKED`].
pub fn value(name: &str) -> Option<String> {
    resolve(std::env::var(name).ok().as_deref(), baked(name))
}

/// [`value`]'s rule on values a caller already has: a non-blank environment
/// value, trimmed, wins; otherwise the baked one. Pure, so the rule is
/// testable without touching the process environment.
fn resolve(environment: Option<&str>, baked: Option<&str>) -> Option<String> {
    environment
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(baked)
        .map(str::to_owned)
}

/// The Koru Cloud this build signs in to, if it has one:
/// `KORU_PUBLIC_API_ORIGIN` from the environment, else what the release baked.
pub fn cloud_origin() -> Option<String> {
    value(name::KORU_PUBLIC_API_ORIGIN)
}

/// **Whether this build has a Koru Cloud at all — the one fact.**
///
/// An official build bakes its origin; a community build, or a development
/// launch with no `.env` file, has none. Without one there is nothing to sign
/// in to: Koru Agent never starts its account capability, and every surface
/// that offers sign-in, an account or Cloud sync says *Koru Cloud isn't set up
/// in this build* rather than that Koru Cloud could not be reached. That is
/// not an outage and nothing retries it. Every reader asks this function, so
/// no screen keeps a second opinion.
pub fn cloud_configured() -> bool {
    cloud_origin().is_some()
}

/// The human listing — what `koru.exe --envs` and `koru.bat envs` print —
/// grouped by scope, with the current session's value beside anything set.
pub fn table() -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for scope in [Scope::Runtime, Scope::Compile, Scope::Script] {
        let _ = writeln!(out, "{}", scope.heading());
        let _ = writeln!(out, "{}", "-".repeat(scope.heading().len()));
        for var in VARS.iter().filter(|var| var.scope == scope) {
            let current = match std::env::var(var.name) {
                // A credential's value is the one thing this listing must not
                // render — see `EnvVar::secret`. Whether it is set is the
                // useful half and is safe to say.
                Ok(_) if scope == Scope::Runtime && var.secret => {
                    "  [currently set, hidden]".to_string()
                }
                Ok(value) if scope == Scope::Runtime => {
                    if var.kind == Kind::Switch {
                        format!(
                            "  [currently {}]",
                            if switch(var.name) { "ON" } else { "off" }
                        )
                    } else {
                        format!("  [currently {value}]")
                    }
                }
                _ => String::new(),
            };
            let _ = writeln!(out, "  {:<24} {}{}", var.name, var.values, current);
            let _ = writeln!(out, "      {}", var.purpose);
            let _ = writeln!(out, "      affects: {}", var.affects);
        }
        let _ = writeln!(out);
    }
    out.push_str(
        "Details and the truthiness rule: reference/ENV.md
",
    );
    out
}

/// The whole of `reference/ENV.md`. Never hand-edited — see the banner it writes.
pub fn markdown() -> String {
    markdown_for(
        VARS,
        "Environment variables",
        "crates/koru-env/src/lib.rs",
        "KORU_UPDATE_ENV=1 cargo test -p koru-env",
        "Switches obey one rule: **on** when set to anything except `\"\"`, `0`,\n\
         `false`, `no`, `off` (case-insensitive). `koru.bat envs` prints this\n\
         table with the current session's values; `koru.bat setup-envs` walks\n\
         the runtime switches one by one.\n",
    )
}

/// Render a table of variables as a generated page. `VARS` is one such
/// table; a registry outside this crate (ADR-456: the hosted services'
/// `SERVER_VARS`) renders its own page through the same function, so the two
/// pages cannot drift in shape.
pub fn markdown_for(
    vars: &[EnvVar],
    title: &str,
    source: &str,
    regenerate: &str,
    intro: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(&format!(
        "<!-- Generated from {source}. Do not edit by hand;\n"
    ));
    out.push_str(&format!("     run `{regenerate}` to regenerate. -->\n\n"));
    out.push_str(intro);
    for scope in [Scope::Runtime, Scope::Compile, Scope::Script] {
        if !vars.iter().any(|var| var.scope == scope) {
            continue;
        }
        out.push_str(&format!("\n## {}\n\n", scope.heading()));
        out.push_str("| Variable | Values | Purpose | Affects |\n");
        out.push_str("| --- | --- | --- | --- |\n");
        for var in vars.iter().filter(|var| var.scope == scope) {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                var.name, var.values, var.purpose, var.affects
            ));
        }
    }
    out
}

/// **The source sweep every registry's drift test uses** (ADR-456).
///
/// `koru-env`'s test holds `VARS` to the packages in this repository. The
/// hosted Koru Cloud services, which are not in this repository, hold their
/// own table to their own packages with the same function, so the two
/// registries cannot disagree about what a read is.
pub mod drift {
    use std::collections::BTreeSet;
    use std::path::Path;

    /// Every `KORU_*` name read by the `.rs` files under `root/crates/*` and
    /// `root/apps/*`, for each top-level directory `include` accepts. It is
    /// told the directory and whether that package is proprietary
    /// ([`is_proprietary`]).
    pub fn reads(root: &Path, include: impl Fn(&Path, bool) -> bool) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for top in ["crates", "apps"] {
            let Ok(entries) = std::fs::read_dir(root.join(top)) else {
                continue;
            };
            for entry in entries.flatten() {
                let dir = entry.path();
                if dir.is_dir() && include(&dir, is_proprietary(&dir)) {
                    walk(&dir, &mut out);
                }
            }
        }
        out
    }

    /// Whether the package in `dir` is one of the hosted services: its
    /// manifest says `license = "LicenseRef-Koru-Proprietary"`. No package in
    /// this repository does; the hosted services' own sweep uses it.
    pub fn is_proprietary(dir: &Path) -> bool {
        std::fs::read_to_string(dir.join("Cargo.toml")).is_ok_and(|manifest| {
            manifest.lines().any(|line| {
                let line = line.trim();
                line.starts_with("license") && line.contains("\"LicenseRef-Koru-Proprietary\"")
            })
        })
    }

    fn walk(dir: &Path, out: &mut BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !path.ends_with("target") {
                    walk(&path, out);
                }
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                reads_in(&content, out);
            }
        }
    }

    /// `KORU_*` string literals within a short window after an env-reading
    /// form: `env::var(`, `var_os(`, `option_env!(`, `set_var(`,
    /// `remove_var(`, `rustc-env=`, `rerun-if-env-changed=`. The window
    /// survives rustfmt's line breaks; prose mentions in comments and log
    /// strings do not match because they follow none of those forms. A
    /// `name::KORU_*` constant path is a read in its own right.
    pub fn reads_in(content: &str, out: &mut BTreeSet<String>) {
        const FORMS: [&str; 8] = [
            "env::var(",
            "var_os(",
            "option_env!(",
            "set_var(",
            "remove_var(",
            "rustc-env=",
            "rerun-if-env-changed=",
            // The module-local name-constant idiom: `const ENV: &str = "KORU_…"`.
            ": &str = ",
        ];
        for form in FORMS {
            for (index, _) in content.match_indices(form) {
                let mut end = (index + form.len() + 80).min(content.len());
                while !content.is_char_boundary(end) {
                    end -= 1;
                }
                let window = &content[index..end];
                let Some(start) = window.find("\"KORU_") else {
                    continue;
                };
                insert(&window[start + 1..], out);
            }
        }
        // Migrated call sites read through a registry's name constants —
        // `koru_env::switch(koru_env::name::KORU_JANK)` — with no string
        // literal at all.
        for (index, _) in content.match_indices("name::KORU_") {
            insert(&content[index + "name::".len()..], out);
        }
    }

    fn insert(text: &str, out: &mut BTreeSet<String>) {
        let name: String = text
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
            .collect();
        if name.len() > "KORU_".len() {
            out.insert(name);
        }
    }
}

#[cfg(test)]
mod tests;
