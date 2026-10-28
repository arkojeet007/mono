# Koru — orientation for a cold start

This is the orientation for everyone who works on the code, people and coding
agents alike. Building and running from a fresh clone is in
[README.md](README.md); how to send a change is in
[CONTRIBUTING.md](CONTRIBUTING.md).

Koru is a workspace: **a tree of typed nodes** — tasks, documents, contacts,
images — with attributes that are data rather than text. **Views are projections
over that model.** State is stored as Loro snapshots; because Loro is a CRDT two
people converge without a server, but that is a *capability*, not a requirement.
A single user never exchanges an op; the snapshot is simply the file format.

**This file is deliberately short.** It holds only what applies before you know
which part of the tree you are in. Everything else — the crate map, the model,
the library traps, the platform traps, the store, releases — is one hop away in
the table below, and is written where it can be kept true. A fact repeated here
is a fact that drifts: this file and `CLAUDE.md` were maintained as two copies
and had diverged by 786 lines before they were collapsed into one.

## Where to look

**Public user and contributor guides live in [guide/](guide/README.md)** and
are part of every clone. Contribution policy stays in
[CONTRIBUTING.md](CONTRIBUTING.md); generated references stay in `reference/`.

**The internal documentation is under `docs/`, a git submodule** of koru-docs
(github.com/koruworks/docs, private), the team's documentation repository. Its
table of contents is [docs/README.md](docs/README.md), which explains how the
docs are structured and which page to read before touching an area. A team
checkout gets it from `koru.bat setup` (or `git submodule update --init docs`);
in a public clone `docs/` is empty and nothing breaks, because everything the
code needs is in this repository:

| | |
| --- | --- |
| Using Koru and connecting an MCP client | [User guides](guide/user/README.md) |
| Building, testing, Rust, and dependency behavior | [Contributor guides](guide/contributing/README.md) |
| How document semantics cross the MCP boundary | [crates/koru-mcp-server/MCP.md](crates/koru-mcp-server/MCP.md) |
| Every env var — **generated, never hand-edited** | [reference/ENV.md](reference/ENV.md) |
| Every generated reference — env vars, events, shortcuts, the API and wire schemas — and how to regenerate each | [reference/](reference/README.md) |
| **Licensing — MPL-2.0, the SPDX headers, what is proprietary, third-party attribution** | [licensing/](licensing/README.md) |
| How third-party code is fetched and patched, never vendored | [third-party/](third-party/README.md) |

## Before you edit anything in an area, read that area's page

The expensive knowledge in this repo is *what a library does that its
documentation does not say*, and it is filed by area rather than repeated here:

- **Touching text layout, rendering or input?** [guide/contributing/deps/](guide/contributing/deps/README.md)
  first. parley has no inline-box API and places boxes on the baseline; loro has
  no `unmark_utf8` and indexes in unicode scalars where parley uses UTF-8 bytes;
  vello has no mipmaps and no `Mix::Clip`; winit's undecorated window keeps
  `WS_SIZEBOX` but loses its resize border, Ctrl+I arrives as `NamedKey::Tab`
  on Windows, and winit *owns* the macOS `NSApplication` delegate — replacing it
  aborts the process on the first event.
- **Touching the store, the gateway or sync?**
  [docs/architecture/STORE.md](docs/architecture/STORE.md). An acknowledgement
  means the bytes are on the disk; a checkpoint prunes only what the *previous*
  one covered; a delta whose causal ancestors are missing imports `Ok` and
  applies nothing.
- **Creating or changing UI?** Start at [docs/features/koruui/](docs/features/koruui/README.md) and follow
  its reuse-first [contribution rules](docs/features/koruui/CONTRIBUTING.md), including
  per-platform availability and approval of a new shared contract before it
  lands — proposed in a public issue, as [below](#proposing-a-change-and-recording-a-decision).
- **Touching chrome?** [docs/features/design/DESIGN.md](docs/features/design/DESIGN.md), and every floating
  surface goes through `scene::floating_panel`.
- **Adding a source file?** It starts with `// SPDX-License-Identifier: MPL-2.0`
  (or the language's comment form); `python scripts/checks/spdx-headers.py` adds it,
  and `--check` lists what is missing. Never on third-party or generated
  code, a test fixture, or a SQL migration —
  [licensing/](licensing/README.md) says why.
- **Adding third-party code?** Do not commit what can be fetched. A crates.io
  crate is a dependency; one Koru must change is a `crate` line and a patch,
  and an upstream file used as it is is a `file` line, both in
  `third-party/fetch.manifest` ([third-party/](third-party/README.md)). A
  binary is committed only when it is Koru's own and nothing can produce it.
- **Adding a shortcut?** It belongs in `koru-keys`' `REGISTRY`, and it is
  printed through `show_hint` — never written out at a call site.
- **Adding a `KORU_*` variable?** It belongs in `koru-env`'s `VARS`, or it is
  invisible to `koru.bat envs` and to [reference/ENV.md](reference/ENV.md).
  `koru-env` holds only variables that code in this repository reads: its
  drift test is two-way, so a row nothing here reads fails it, and so does a
  `KORU_*` read with no row. Variables only the hosted Koru Cloud services read
  are declared with those services, outside this repository.

A test enforces the last two. The rest are enforced by somebody reading the
page, which is why the page is one hop from here and not thirty lines of summary
that goes stale.

## Responsiveness is a product requirement, not an optimisation pass

Typing, switching tabs and moving the window have to feel instant. Treat a
change that makes any of them slower as a bug, not a trade.

`Editing::reflow` is the expensive call — it lays out *every* row through parley
and is O(rows). Almost every rule about performance in this repo is a rule about
not calling it: only the active tab draws, only the active tab reacts, a rebuild
re-reads only what changed, an animation never reflows, nothing writes to disk
or walks the document on a timer, and opening is O(the document) rather than
O(its history).

The measured cases, the numbers and the one place this is still broken are in
[docs/contributing/operations/RUNBOOK.md](docs/contributing/operations/RUNBOOK.md) and [docs/deferred/](docs/deferred/README.md).
Measure rather than assume: `KORU_FRAME_STATS=1`, `KORU_STARTUP_STATS=1`, and
`./koru.sh jank` for a freeze mid-session. All three are free when off.


## Testing

The suite is the spec. **Run it as `cargo test --workspace`** — which is what
`./koru.sh test` and `koru.bat test` do. Both halves are capped in
`.cargo/config.toml` and neither launcher passes a flag; an explicit
`-- --test-threads=N` still wins.

- **Tests live beside the module they cover**, as a `#[cfg(test)] mod tests;`
  child file, so private items stay reachable through `use super::*`.
- **Tests must not touch disk**, must not reach the store, and must not open a
  browser. Each of those has been broken once and each is now held by a feature
  flag or a `cfg` — `no-reader`, `no-browser` — because **a dependency is never
  compiled with `cfg(test)`**, so a guard inside a crate does not cover the
  crates above it.
- **Tests must not send telemetry**, which is the `no-reader` lesson a third
  time. An events worker can construct its sink without being configured, so
  a suite that opens a real worker would post its fixtures to the telemetry
  service. `cfg!(test)` inside `koru-events-server` covers that
  crate and nothing above it, so the crates that open a worker turn on
  **`no-sink`** in their `[dev-dependencies]`.
- **Never assert that a connection fails.** "No store runs under `cargo test`"
  passes on CI and fails on the machine of anyone with Koru open. Arm the state
  you mean to test instead.
- **A test run can leave agents and stores behind**, and they hold the exe
  locks. The tell is a `koru-*` process whose command line names a `%TEMP%`
  path; `koru.bat stop-all` clears this checkout's.

What lives where, and the process-global state that needs a lock, is in
[guide/contributing/TESTING.md](guide/contributing/TESTING.md).


## Losing content is the one unrecoverable bug

The app is now used to write things that are not in git. Four invariants exist
only to keep it that way, and each one is easy to undo without noticing:

- **An argument that is not understood is not run.** `koru_workspace_host::cli::parse`
  is strict: it names the token, suggests the nearest flag within three edits,
  prints the usage and exits 2. Ignoring an unknown argument sounds harmless
  and is the same failure as guessing a directory — a mistyped `--profile`
  silently opens the *last-selected* workspace instead of the named one. It sits
  in `run` rather than a binary's `main` so all three binaries get it; two of
  them accepted anything at all before. `FLAGS` is the only table, and
  `every_documented_flag_parses` fails if the parser and the help drift apart.
- **Content is in the store; a plain launch never asks for or opens a profile
  folder.** Profile-independent settings use the shell namespace until a real
  Store Profile exists. An empty Profile catalog returns `NeedsProfile` before
  any Profile/Workspace document or id is constructed; a Store that will not
  answer is fatal. Explicit legacy folder import/open remains available
  ([ADR-86](docs/decisions/README.md)).
- **There is no in-memory fall-back on a failed open.** `Session::open` returns
  `Err`, the shell shows a dialog and exits. The old code opened a blank
  workspace that could not save and said so only on stderr — which, with no
  `windows_subsystem` and usually no console, is silence. An empty window must
  only ever mean an empty workspace. A window with no Profile is instead the
  explicit `NeedsProfile` onboarding state described above; it does not enter
  the full app until disk-backed creation succeeds.
- **A recovery is a value, not a log line.** `WorkspaceStore::load` returns
  `Opened::{Fresh, Normal, Recovered}` so a caller cannot ignore it by accident,
  and quarantined files are **copied, never moved** — recovery must not be able
  to destroy the thing it was called to preserve.
- **An older build never writes a newer document.** `Compatibility` gates on
  `schema_version`, which was written from the beginning and read by nothing
  until now. A save is a full re-export of a document the older build only
  partly understands, so read-only is the smaller failure — and a visible one.

Two smaller ones with the same character: `assets/` beside the exe is searched
**before** the compiled-in `CARGO_MANIFEST_DIR` paths, or a bundle missing a
font looks perfect on the machine that built it; and `sync_in` skips a peer log
it cannot decode rather than failing the load, so one damaged transport file is
not a refusal to open a document sitting intact in the snapshot.

What is *not* fixed — no `fsync`, a shared `snapshot.loro.part`, a non-atomic
append in `publish`, no instance lock — is written up under
[Storage and sync](docs/deferred/README.md). Do not assume they were missed.

## What this repository holds, and what it does not

This repository holds the desktop and mobile apps, every crate they link, the
wire contracts and their test vectors, the release tool, Koru's patches to the
third-party code it builds with (fetched, not vendored:
[third-party/](third-party/README.md)), the generated references and the
licensing audit. Public guides are in `guide/`; internal documentation is in
the private `docs/` submodule. **Koru
Cloud — the hosted service official builds sign in to — is not here**, and
neither are its infrastructure or any credential. The dependency runs one way:
code outside this repository may depend on it, and nothing here names anything
outside it — no path dependency, `include_*!`, test fixture or build script
reads a file this repository does not hold. A build from this source alone must
compile, pass `cargo test --workspace`, run and package. Everything tracked
here is public, so nothing classifies it;
[ADR-459](docs/decisions/ADR-459-noboundarytooling.md) says what was removed
when the split from the private tree was done.

Configuration that only an official build has (the Cloud origin, the
update-signing key, the Google Docs importer's registration) is absent here by
design, and every feature that needs it must work, or say plainly that it is
not set up, without it. [CONTRIBUTING.md](CONTRIBUTING.md) says how to supply
your own.

## Proposing a change and recording a decision

- **Contributions come in under MPL-2.0**, with no contributor license
  agreement and no sign-off line. [CONTRIBUTING.md](CONTRIBUTING.md) says what
  sending a change certifies, including for code written with an AI assistant.
- **A proposal starts as a public issue or discussion.** Anything with a large
  blast radius — a new shared KoruX component or contract, a change to the data
  model or a stored format, a new dependency, a new process or service —
  is agreed there with a maintainer before the code is written. A small fix
  can go straight to a pull request.
- **An accepted decision becomes an ADR** in `docs/decisions/`, landed with the
  code it explains. The ADRs are part of the internal docs, so for an outside
  contribution a maintainer records the decision; the issue and the pull
  request carry the reasoning in the meantime.
- **Never take a product decision silently.** If a change alters what a user
  sees or how the product behaves, say so in the pull request and let a
  maintainer decide.
- **A schema change is announced, not discovered.** Bumping a store schema
  version or a document's `schema_version` migrates every database the new
  build opens, and older builds then refuse it. Say "this changes the database
  schema" in the issue first, and run the first build against an isolated
  root (`--root`, or `KORU_ROOT`; see the [RUNBOOK](docs/contributing/operations/RUNBOOK.md)).

## Keep the docs with the code

**The docs are part of the change, not a follow-up.** Public guides are split
by reader under [guide/](guide/README.md) and are changed in this repository
alongside the code. The internal map is [docs/README.md](docs/README.md).
A change to a page under `docs/` is a commit in the submodule, pushed
to koru-docs, and then a commit here that moves the `docs` pointer to it; land
both, or the pointer names a page change nobody else can fetch. Which page a
change updates is in [docs/README.md](docs/README.md#keep-the-docs-with-the-code).

**An issue, a plan or a pull request description is not a permanent home.**
They describe what is *about* to change; the pages under `guide/` and `docs/` describe what
*is*. When a change settles something — a data-model change, an architectural
shape, a user-visible behaviour, an operational step — it updates the page a
reader would look for it on (`DATAMODEL.md`, `docs/architecture/`,
`guide/user/`, an ADR, …) in the same change. A fact whose only copy is
in a closed issue or a merged pull request is invisible to the next person,
and anything that is now simply how Koru works belongs under the
reader-oriented guides or internal documentation tree, according to its audience.

Two habits that make this cheap rather than a chore:

- **Write the ADR when the decision is made**, while the alternatives are still
  in your head. Reconstructing "why not the obvious thing" a week later is how
  reasoning gets lost.
- **Record the surprising thing.** A library that does not behave as documented,
  a limitation found by experiment, an API that had to be verified — that is the
  content nobody can rederive. Put the guessable parts in doc comments instead.

Renaming or moving a doc means grepping the whole tree, not only `docs/`: the
Rust source cites docs by path in hundreds of places. Then
`python scripts/checks/oss-links.py .` lists every Markdown link the move broke.

## Working in this repo

- **Spell it `color`, not `colour`.** New identifiers, new comments and every
  string a user reads. The codebase was written in British English and most of
  it still says `colour` — that is history, not a second convention, and a
  mixed file is fine while the sweep has not happened. **Two things never
  move**: `NodeMeta.colour` and the `hl` mark's stored names are *stable
  storage* ([DATAMODEL.md](docs/architecture/DATAMODEL.md)), and renaming a storage key
  orphans every document already written with it.
- `cargo fmt` reformats aggressively, so scripted search-and-replace patches
  break on whitespace. Prefer targeted edits.
- **Kill a running instance before building** — Windows locks the exe.
- Heredocs mangle `\n` inside Rust string literals. Use the editing tools rather
  than shell-scripted patches for anything containing escapes.
- There are **no `**bold**` delimiters in the text** — emphasis is a Loro mark.
  Anything that greps for `*` in a line is looking at the old model.
- **`scripts/` is what the launchers run**, in four folders: `setup/` fetches
  and builds what a fresh clone lacks and checks the machine, `dev/` starts,
  stops, captures and probes the app, `checks/` holds the repository's checks
  (links, the publication scan, SPDX headers), and `icons/`
  regenerates the icons from the logo.
- **The skills come from [koruworks/skills](https://github.com/koruworks/skills)**,
  not from this tree: `scripts/setup/setup-skills.sh` runs `npx skills add` into
  `.agents/skills/` (which `.claude/skills` links to), and `.githooks/post-checkout`
  runs it in every new worktree. A change to a skill is made there, not by
  editing the installed copy, which the next `--update` replaces.
- **At the start of a session, make sure the skills are current.** Run
  `sh .agents/check-skills.sh` (`.agents\check-skills.bat` from cmd or
  PowerShell) unless your harness already ran it for you (Claude Code does,
  through `.claude/settings.json`). It prints one line when
  `.agents/skills` is behind koruworks/skills, and nothing otherwise. Tell the
  user and ask before updating; on a yes, `sh .agents/check-skills.sh --update`.

## Maintainers' private notes

The maintainers keep their internal process — how they review and sign off
plans, the hosted service's pages, and their own agent tooling — in a private
file, `AGENTS.private.md`, that is not part of this repository. If it sits
beside this file in your checkout, read it after this one; it adds to this
file and never overrides it. If it is absent, nothing here depends on it.
