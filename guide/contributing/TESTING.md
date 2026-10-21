# Test architecture

The schedule SSE drop regression holds its fake bridge open with a channel
until `Drop` returns, proving cancellation does not join the bridge. It does
not measure a 100 ms wall-clock deadline: the former fake held the cancellation
mutex through a 250 ms sleep, so reading the cancelled flag could itself exceed
the deadline after a correct, immediate drop.

Mobile Run compatibility fixtures use `RUN_DOCUMENT_SCHEMA_VERSION + 1` for
a newer document. A literal `2` became a supported document when scheduled Run
provenance raised the current version, leaving the fixture testing the opposite
of its stated newer-build case.

Where the suite lives and what each part covers.

`koru-sqlite`'s four file-backed tests and `koru-free-space`'s three volume
tests are ignored by default: they create temporary files and query the host's
filesystem. They still compile in ordinary workspace tests; SQLite's two
in-memory migration tests still run. Run the physical checks explicitly with
`cargo test -p koru-sqlite -p koru-free-space -- --ignored`. Each owns a fresh
temporary directory. Reading back SQLite pragmas proves configuration, not
survival of a power cut; free-space results exercise only the host platform.
The missing-path fixture has platform-specific expectations: Windows'
[volume-path resolution](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getvolumepathnamew)
accepts a missing final component on an existing volume, while Unix refuses
the missing path. A free-space query must not double as an existence check.

The hosted Cloud service's own tests — its PostgreSQL, disk, worker and
publication fixtures — live with that service and are not in this
repository. What is here covers the client and shared halves.

Cloud record duplex tests (`crates/koru-network-iroh/src/cloud/`) use bounded memory streams and output buffers. They
cover crossed large records, event-wait cancellation after partial I/O, retained
receipt ownership, EOF/explicit cancellation, and absolute deadlines. Two
regressions reproduce ready I/O winning after expiry; the wrapper now checks
expiry first. These prove transport ownership, not worker activation.

Cloud admission preparation tests use memory streams to check verified input,
unchanged whole-handshake deadlines, retained-context disposal, credential
expiry during preparation, and immediate scoped fencing during an idle finish.

Cloud worker endpoint tests use actual loopback UDP and the existing in-process
HTTP relay. They exercise fixed binding, configured advertisement, signed
Profile/Workspace admission and replacement streams, endpoint teardown/rebind,
and a relay-only client with no UDP transport. Current authorization remains an
explicit fixture and document transfer remains gated. These tests touch no disk,
Store, public relay, provider or telemetry.

The disconnected Plan 20 object candidate's tests live beside
`koru-replication/src/cloud/object` and its codec/identity/causal modules. They
consume raw pinned object/Loro fixtures, independent expected metadata and
real in-memory model readers. Actual malformed encodings, pending ancestors,
checkpoint mode, external legacy document identity and Profile scope are
tested without Store, I/O or telemetry. Declaration bounds are not allocated
capacity; fresh causal content is not SQL/disk/worker evidence. Historical
artifact source pins remain immutable rather than being relaxed for new code.

Koru Voice activation adds three pure boundaries. koru-voice-ui tests its model,
shared draw/hit geometry, keyboard behavior, every language, and real raster
output at representative scales without opening a microphone, Store, browser,
vault, or model cache. Agent hotkey tests use an in-memory registrar and
command queue, including pre-ready enqueue, post failure, rapid replacement
and stale generations. Coordinator tests fake RestoreHint, accessibility,
Voice and insertion, proving preparation performs no UIA work and a focus race
refuses before Begin. Real RegisterHotKey, UIA restoration, secure desktop and
application compatibility remain machine acceptance.

Plan 1.6 adds colocated Voice HUD tests. koru-voice-ui pins all four lifecycle
states, every language, representative scales, real WGPU pixels, 0/8 meter
boundaries, shared draw/hit geometry, and proves level-only updates reuse label
layout. Agent's voice_window tests pin Settings precedence, one terminal
deadline, active-state replacement, physical-position/logical-size mixed-DPI
geometry, and counters showing no creation, draw, redraw, deadline, animation,
or level work while hidden. koru-window pins that its passive style preserves
existing extended styles. An ignored interactive Windows harness uses the real
UIA backend and a minimal native NOACTIVATE popup to re-prove the exact root,
PID, and focused element through native reveal, updates, click, and hide. It
does not exercise VoiceWindowHost, the GPU reveal, Agent snapshot dispatch, or
real HUD pixels/Finish; Plan 1.7 owns that product-path proof.

Accessibility (Plan 1a.1) is split the same way, and the split is the point.
Everything above the platform runs in `cargo test --workspace` against
`koru_window::AccessibilityProvider`, a trait whose real implementation needs
an event loop, a window and an attached screen reader: identity that survives a
rename, a reorder and all eight languages; two windows never answering each
other's ids; a closed menu not handing its queued action to the one that
replaced it; and a counter proving a caret blink or a UIA query builds no tree.
`accessibility::tests::every_window_the_host_creates_is_accounted_for` keeps the
reveal audit true by counting the window-creation sites in `app.rs`, because a
provider can only be installed while a window is still hidden.

What that cannot establish is whether an *external* client sees any of it.
`crates/koru-workspace-host/tests/accessibility_native.rs` is the separately
invoked lane — `cargo test -p koru-workspace-host --test accessibility_native
-- --ignored` — and it is a real UI Automation client in the COM multi-threaded
apartment: it reads the window root's `AutomationId`, finds a control by
`FindFirst` on that property, and invokes it. It also sets
`KORU_ACCESSIBILITY_DUMP` before it starts and asserts that a real client
attaching produces **exactly one** snapshot dump with no accessible name in it —
the only path a person will ever take to one, and the only place it can be
tested end to end. It opens a real window, which is why it is `#[ignore]`d. It proves nothing about what a screen reader *says*;
Narrator and NVDA runs are a person's, by hand.

The save status (Plan 1b.4) is split three ways, and each layer proves
something the one above it cannot. `crates/koru-workspace/src/durability/`
drives **two real `StoreSession`s over fake links**, acknowledges one, and
asserts the other still says its work is owed — *another document's
acknowledgement cannot establish success*, tested against the Store's own
bookkeeping rather than against a struct's fields. `apps/koru/src/mainview/
accessibility/persistence/` arms each of the seven states as a plain
`Durability` value through the real `View::set_durability` seam and asserts the
sentence a screen reader would be given, that the ordinary saving cycle
publishes no announcement at all, that a failure and a recovery each change the
announcement exactly once, and that no semantic query takes a durability
reading of its own. Neither layer can establish that a live region reaches a
client, so the interactive lane above now also registers a real
`IUIAutomationEventHandler`, changes one label and asserts the event arrives —
then republishes the identical label twice and asserts silence. Whether
Narrator or NVDA chooses to *speak* a polite region is a person's check, and
nothing automated stands in for it. The live reopen/restart retention proof
needs a real Store on a real root, which ordinary tests must not touch: it is
checked by hand.
`crates/koru-workspace-host/tests/accessibility_onboarding_native.rs` is the
same lane for the **first-run** window, and it hosts the real `OnboardingView`
rather than a fixture: a client finds *Continue offline* by a locator it builds
itself, presses it, and then checks that the welcome screen's control is no
longer findable while the Profile step's is. Two things about AccessKit were
established there rather than by reading the crate — that a `Role::Label`'s
accessible name comes from its *value*, and that live regions work on Windows
and follow the name. Both are in `guide/contributing/deps/ACCESSKIT.md`.

## Testing

Plan 13A's colocated account-session and account-wire tests consume the frozen
issuance vectors through real signing, the existing purpose-isolated verifier,
strict DTO decoding and the actual rate subject/encoding/crypto path. The rate
counter/window backend is an explicit in-memory port. Signature tests preserve
all bound claims and distinguish the 600-second producer from the existing
900-second verifier ceiling; whole-second expiry overflow cases do not imply
that the signer accepts timestamps outside the verifier's millis domain.
Response tests explicitly separate structural refusal from checks requiring
verified claims/current route context. No new test starts a Store, reaches a
database/provider, sends telemetry or claims HTTP issuance/worker/mobile proof.

Account creation's runtime fixtures use fake HTTP, clock, SDK, vault and
browser listeners to prove exact decision/exchange retry through cancellation
and uncertain responses. The hosted side's own fixtures live with the hosted
service. None of these claims a real browser, Google account, OS vault or
physical-device journey. Rust and Kotlin decode the same generated
confirmation/cancel command fixtures.

`koru-mobile-host/src/account/tests` exercises the platform mailbox with injected
credentials, service, clock and inventory rows; the composed native fixture uses
the production account service with fake HTTP and SDK/vault responses. These
tests never open a Store, vault, browser or on-disk database. Runtime's explicit
`test-support` feature exposes its in-memory lifecycle journal and trust repository to that
composition. Rust and Kotlin consume the same account command fixtures under
`contracts/testdata`; generated Swift fixture execution remains I6 unrun.

`koru-mobile-host/src/trust/tests.rs` uses the actual in-memory shared trust import
transaction and fake legacy-file/archival effects. It checks signed QR grants,
revocations, preserved identity/revision, corrupt-source refusal and startup
after commit-before-rename interruption. Runtime migration tests inject a SQLite
trigger failure after insertion and prove rollback leaves no conversion marker.
Existing enrollment commit/replay/JTI/reconciliation tests remain shared.

Plan 1.10 directory fixtures construct the full frozen six-table schema 4 in
memory, seed real signed QR/enrollment records and replay bytes, then compare
schema 5 with a fresh repository and every preserved row. They exercise
monotonic version fences, lost-ready acknowledgements, namespace/active-pointer
foreign keys, independent legacy retirement and transactional target-set
rollback. Credential persistence tests inject vault/directory failures and
termination boundaries, including an older same-account Auth Identity surviving
a replacement attempt. Hosted inspection fixtures compare every session field
before/after current-secret success and previous-secret/device/owner/state
refusals, count one session read, and prove source limiting precedes storage.
The inspection route emits no activity fact and does not exercise refresh.
Android tests preserve independently seeded v1 ciphertext compatibility and
v2 logical-entry authenticated-data isolation with injected wrapping keys.
None of these tests uses real vault storage, Google, Store content or physical
upgrade/QR/Apple execution; those remain separate acceptance evidence.

The suite is the spec — thousands of tests, with no GPU required for the full
workspace run. The exact count changes with each landed plan; obtain it from
`cargo test --workspace -- --list` filtered to `: test`. What lives where,
and why:

| Location | Covers |
| --- | --- |
| `crates/koru-core/tests/convergence.rs` | two replicas merging: text, marks, pills, tree shape, the orphan policy, undo semantics |
| `crates/koru-core/src/explore/tests.rs` | the Explore projection: ids, tree order, ordinals, the move translation |
| `crates/koru-core/tests/structural_ops.rs` | split, merge, indent, reorder |
| `crates/koru-core/tests/query.rs` | `TaskQuery` filters and `TaskSummary` resolution |
| `crates/koru-core/tests/text.rs` | marks, runs, the anchor, `Utf8Pos` |
| `crates/koru-core/tests/schema_version.rs` | which build may write to which document |
| `crates/koru-docstore/src/lib.rs` | recovery, quarantine, backup rotation, a damaged peer log |
| `crates/koru-workspace/tests/opening.rs` | what a session reports, and what read-only refuses |
| `crates/koru-workspace-host/src/cli.rs` | every accepted flag, and what a rejected one says |
| `apps/koru/src/tests.rs` | the key map — see ADR-17 |
| `apps/koru-agent/src/reminders/tests.rs` | deterministic reminder reconciliation, cursor safety, floating-zone resolution, missed policy, activation validation and fake native diffs |
| `crates/koru-account-runtime/src/account/tests.rs`, `account/lifecycle.rs`, `enrollment/tests.rs`, `identity/tests.rs` | moved account, lifecycle, enrollment and SQLite trust expectations; fake hosted calls and vault, dev-only loopback callback fixtures, isolated temporary trust databases |
| `apps/koru-agent/src/account/delivery/tests.rs`, `identity/tests.rs` | desktop callback conversion and browser-response bytes without opening a browser; retained protected-key adapter tests |
| `crates/koru-editor/src/rowlayout.rs` | inline boxes land at their anchors |
| `crates/koru-editor/src/paragraph.rs` | cluster and word motion, and where a line ends |
| `crates/koru-editor/src/accessibility/tests.rs` | the index mapping between Koru bytes, parley clusters and AccessKit characters, against **real** layouts: every caret position round-tripping in Hangul, Cyrillic, a decomposed Latin letter and a skin-tone emoji; the layout strut never reaching a screen reader; an empty paragraph publishing one empty run; run ids stable across publishes and distinct between paragraphs |
| `crates/koru-editor/src/accessibility/projection/tests.rs` | what a publish *costs*, against a real 1,000-paragraph document and a real reflow: activation is O(the document) and a keystroke, a caret move, an idle frame and a scroll are not; an off-screen paragraph is discoverable and reveals its text when scrolled to; no run links to a run that was never published; RTL and mixed-direction paragraphs are covered exactly by their runs; a 2,000-character unbroken run is split below the `word_starts` cap |
| `crates/koru-editor/src/accessibility/actions/tests.rs` | what an assistive technology may ask of a paragraph and what it may not, against a real `Editor`: a focus request that moves the keyboard and **not** the caret, a selection that lands on exactly the bytes it named, a range naming runs the view never published refused rather than clamped, a range belonging to another paragraph unable to truncate this one, and — the shape of the read-only guarantee — every action a client can send leaving the document's bytes and its Loro version untouched |
| `crates/koru-editor/src/accessibility/events/tests.rs` | that every update a real editing session produces is coherent and says something (a keystroke, a split, a merge, an undo, a redo), and that each of the four wrong shapes is refused: a selection naming a run this update removes, one naming a run nothing published, one reaching into another paragraph, and a paragraph both published and removed |
| `apps/koru/src/mainview/accessibility/document/tests.rs` | the **shipping window** publishing a real document, read back through a client model built from the real `TreeUpdate`s: movement by character, word, visual line and paragraph agreeing with the published caret; an AT `SetTextSelection` followed by real typing; split, merge, undo, redo; the five §5 peer cases through the real in-memory CRDT import path; both halves of the peer-edit speech policy; a departed paragraph refused; and the counters — a keystroke publishing 3 node ids of a thousand paragraphs and shaping 1, a caret move 1 and 0, an idle pass none |
| `apps/koru/src/mainview/accessibility/document/native.rs` | `#[ignore]`d, and **inside the binary** because `apps/koru` is a `[[bin]]`. A real container window hosting a real `MainView`, and a real out-of-process UI Automation client that finds a paragraph by `AutomationId`, reads it through `GetText`, `Select()`s a range, types over it, undoes, and receives the native text and selection events. Plan 1b.3's gate; the manual half is `docs/features/accessibility/README.md` |
| `apps/koru/src/mainview/accessibility/integration.rs` | **where the surfaces separate plans built meet**, driving a real `PaneAccessibility` and applying the real `TreeUpdate`s into a client model: two windows over one document as two sets of paragraphs; the same document in two tabs; a document vanishing beneath a modal, refused by the host while it stands, and coming back with the caret; a closed menu refused **by the host** and not merely declined by the menu module; the occurrence map settling across ten menu openings and twelve tab switches; a save acknowledgement and a keystroke not republishing each other; and what a menu over a thousand paragraphs costs. ADR-343-retiredsurfaces |
| `crates/koru-workspace-host/src/accessibility/dump/tests.rs` | that a default snapshot dump carries identity, order, geometry, state, actions, selection and counts — and **no label and no document text**, asserted by searching for the fixture's own strings |
| `apps/koru-document/tests/accessibility_text_native.rs` | `#[ignore]`d. A real window, a real `Editor` over a synthetic fixture, and a real out-of-process UI Automation client reading text, checking the caret, selecting, typing over the selection and **receiving** the two native text events through a registered `IUIAutomationEventHandler`. Plan 1b.1's gate; the verdicts are in `docs/features/accessibility/TEXT-CAPABILITY.md`. Plan 1b.2 added T9 — the window is asked for a dump and the same update is then measured through UIA, so the dump is checked against the platform rather than against itself — and T10, an inline object found by its own locator, named by its own caption, and still there after a keystroke in the same paragraph |
| `crates/koru-workspace-host/src/onboarding/accessibility/tests.rs` | the first-run controller's tree, screen by screen, on the account-free route: every control the keyboard can reach also being in the tree; a transition retiring the previous screen's identities and a stale request being refused rather than routed to the control that replaced it; empty and blank-looking names exactly as the form already handles them; a creation in flight and a failed one announcing themselves; a retry producing one Profile and not two; focus resolving into the tree; identities byte-identical across all eight languages; and the projection raising no typed action and no telemetry |
| `crates/koru-workspace-host/tests/accessibility_onboarding_native.rs` | `#[ignore]`d. A real first-run window hosting the real `OnboardingView`, and a real out-of-process UI Automation client that finds *Continue offline* by a locator it builds itself, presses it, and confirms the screen that went is gone. Plan U0.1's gate; the manual half is `docs/features/accessibility/README.md` |
| `apps/koru-document/tests/accessibility_text_native.rs` | `#[ignore]`d. A real window, a real `Editor` over a synthetic fixture, and a real out-of-process UI Automation client reading text, checking the caret, selecting, typing over the selection and **receiving** the two native text events through a registered `IUIAutomationEventHandler`. Plan 1b.1's gate; the verdicts are in `docs/features/accessibility/TEXT-CAPABILITY.md` |
| `crates/koru-editor/src/components/` | each picker's keys, filtering and hit-testing |
| `apps/koru-agent/src/global_input/tests.rs` | global press/release/timeout/cancel semantics, zero idle work, exact-target refusal, Unicode insertion, direct-insertion refusal/partial outcomes, and privacy, entirely behind fakes. The Windows adapter also has pure native-to-Agent mapping tests in global_input/windows/tests.rs, preserving every refusal, target state and delivery status. Plan 3.1 added the two the extraction needed pinned before anything moved: the platform's **own** re-proof inside `insert_unicode` refusing separately from a rejected or partial count, and a target that goes stale *after* a successful restore still being refused at commit with the platform never reached |
| `crates/koru-windows-input/src/windows/tests.rs` | the pure classifiers, which is all of that file that can be tested without a desktop: the UTF-16 input pairs, the three `SendInput` counts, and what a root handle and a pid can and cannot tell apart. Plan 3.1 added the **limit**, written down rather than left to be found — a root `HWND` recycled inside *one* process passes `classify_restore_hint`, and it is the target capture's `still_same_root` that catches it, because a hint corroborates and the element is the identity |
| `crates/koru-windows-input/src/windows/native.rs` | `#[ignore]`d, and the only test in this tree that sends **real keystrokes**. It creates its own top-level `EDIT` window, proves the foreground and the focused control are ones *it* created in this process, and then drives the shipped mechanism: one synthetic phrase delivered complete and exactly once into that control, read back through the captured element's own `ValuePattern`; a foreground change to a second window of its own making the same target `Changed` and the delivery refused with the control untouched; and a destroyed target refused. **It is not Plan 3.1's baseline** — that needs a real third-party application in an exclusive session and is run by hand;
  live traps are `guide/contributing/deps/WIN32.md`. Windows' foreground lock is what makes it safe to leave here: run from a console that is not in front, it stops at its own guard and types nothing (`guide/contributing/deps/WIN32.md`) |
| `apps/koru-demo/src/{cli,locator,select,supervise,wait,instance,recording}/tests.rs` | the accessibility runner, **entirely pure**: no window, no COM, no disk, no rendezvous. The locator grammar and what `*` may and may not stand for; two matches failing with both candidates named and a candidate printing nothing that could be a document title; a disabled or off-screen control being a different answer from a missing one; a match in a *second* Koru refused by name; waits over a fake clock that spend no time; and the supervisor — control coming back from a provider call that is still running, a fresh apartment answering while the first is still stuck, the abandoned one's late answer never reaching a later call, the abandonment budget, an unblocked worker giving its apartment back, and a drop that does not wait. **None of it proves anything about COM**: a condvar is not a wedged provider, and the file says so. Plan 4.2 pairing tests inject frame timestamps: the first encoded frame is origin, a delayed/dropped frame is not assumed constant FPS, fractional DPI and title-bar height are recorded not applied, a resize fails as `capture-geometry-changed`, required JSON fields are present, and a title stuffed into a locator is dropped. Plan 4.3 outcome tests cover encoder-unavailable, capture-not-ready, finalize failure, target close, cancellation, and that record/test share compiled case steps — none of them open a window or send input |
| `apps/koru-agent/src/dictation_coordinator/tests.rs` | disabled/consent/observer gates, hold and toggle, policy revocation, stale hold flights and Voice generations, worker failure/reconnect, target loss, content-free wake coalescing, and at-most-once final insertion; `koru-dictation` has `no-engine`, so no microphone or worker is reachable |
| `crates/koru-voice-client/src/tests.rs` | real local IPC framing plus deterministic transport interleavings: queued disconnect before dequeue, link retirement after Begin write before binding, and last-owner drop before send; the worker is a socket fixture and opens no microphone |
| `crates/koru-agent-wire/src/tests.rs` | exact protocol JSON/round trips, including v22's content-free routed Dictation gestures and lifecycle states |
| `crates/koru-workspace-host/src/app.rs` tests | routed Dictation same-key adoption versus different/commanded refusal, plus Agent-link loss cancelling without commit and allowing a fresh route |
| `crates/koru-api-client/src/settings/link/tests.rs` | confirmed settings send ownership, exact echo, refusal/disconnect classification, stale generations, safe queued timeout, retry/restart, and result-before-wake ordering |
| `crates/koru-delegate-runtime/src/tests/schedules.rs`; `crates/koru-delegate-sqlite/src/claims/tests.rs` | deterministic schedule planning and execution: run/missed/run, atomic due-plus-representative-miss crash recovery, busy admission, final-clock advance and rollback, post-claim capture retry, per-occurrence context pins and failures, authority edits, handoff ownership, crash reconciliation, duplicate wakes and per-schedule fault isolation |
| `crates/koru-delegate-host/src/schedules.rs` / `scheduler.rs` / `actor.rs` / `roots.rs`; `apps/koru-store-server/tests/api.rs` | exact room selection, reconnect/close behavior and deadline-blocked healthy idle in the host; short pre-claim admission versus per-scope post-claim reservations; noncreating temporary-scope planning; Store integration proves the content-free catalog subscription wakes for create/delete/restore/import while blocking when healthy |
| `crates/koru-settings/src/authority/tests.rs` | startup side-effect/persistence ordering, both compensation directions, double-failure reporting, and explicit retry repair through a pure no-disk seam |
| `apps/koru-agent/tests/gateway_{sync,ai,settings,catalog_events,host_parity,lifecycle}.rs` and `apps/koru-agent/tests/lifecycle.rs` | the **Gateway parity battery** — a real host process on a temporary root, and a real Agent supervising one. **The suites moved here in Plan 7**, out of the gateway crate, when the standalone binary they used to spawn was deleted; they now run against the Agent's worker roles, and `sessions_ipc.rs` and `sessions_switch_latency.rs` arrived the same way from the session host crate. Sync and durable acknowledgements, AI streams, catalog and events, settings, the workspace operation against a real session host, and `gateway_host_parity.rs`: the published discovery record across a restart, the Tasks query port answering through the host, and the account composition staying unavailable. This battery is what the Gateway rehost was compared against, and it stays the contract for the composition the Agent now hosts. **Since Plan 4 no gateway case in `lifecycle.rs` can be skipped** — the child is `koru-agent.exe --worker gateway`, so the image under test hosts the role and there is no sibling to be missing. Three cases were added with the supervision: an unrequested shutdown, asked in its `only_if_idle` form exactly as an authorized caller would ask it, recovered from by the supervisor with **no ensure sent**; the Agent answering a `Status` question and a routed dictation state promptly while its gateway is still starting; and two rooted Agents each supervising their own child |
| `apps/koru-agent/tests/gateway_recovery.rs` | the **recovery battery**. Plan 6 wrote it as a comparison — every assertion run twice, against the standalone `koru-api-gateway.exe` and against a real Agent supervising `koru-agent.exe --worker gateway` — and that was the evidence the rehost changed nothing. **Plan 7 deleted the standalone binary and with it the second arm**, so what is left is not half a comparison: it is the recovery contract of the host that ships, and the assertions are the ones both hosts passed. `Host` starts an Agent on a temporary root, kills whichever process is serving, and waits for the supervisor to put service back. **The kill goes through the published record**: `instance_id` is `"<pid>-<millis>"`, so the pid of the process actually serving is already on the record a client holds, and the fixture owns a `Child` for the Agent and never for the gateway — the honest shape, because the gateway is the supervisor's child, not the test's. Nothing here restarts a gateway; a test that started the replacement itself would be proving its own fixture. The classes: the published record's contract half surviving and its identity half not, with the predecessor's credential refused; a durably acknowledged edit coming back **exactly once**; a confirmed settings write settling **once** and never being replayed onto a value chosen after it; a catalog watch re-established and the activity written before the kill still readable; a workspace removal landing once or not at all; and the Tasks port and the vouched boundary answering identically. Three cases exist only in the supervised shape and say why at the case: a live PTY session surviving a Gateway death (the ADR-280 distinction), two roots recovering independently, and the Agent-and-sessions survival check that every parameterized case calls. What is *not* armable — AI streams under a supervised child, workspace migration without a first-party grant, and killing the sessions worker from outside — is named per case and carried into docs/contributing/release/VERIFICATION.md |
| `apps/koru-agent/tests/gateway_worker_process.rs` and `apps/koru-agent/src/gateway_worker/tests.rs` | the **gateway worker role**: real `koru-agent.exe --worker gateway` children on temporary roots, with the suite playing the Agent at the other end of the private bridge. Exit 2 and the role's own usage — never the Agent's help — for an unknown role, a misplaced `--worker`, an unknown role argument, a flag where `--root`'s path belongs, and a missing or malformed bootstrap; exit 1 for a bootstrap describing another image, channel or root. A valid bootstrap serves, publishes the launch id it was given, leaves the Agent's single-instance endpoint bindable and `<root>/agent/` absent, and stops when the parent lets the bootstrap pipe go. Non-ASCII rooted paths included. The colocated half pins the strict parser and, by reading `main.rs`, that `dispatch` still precedes the Agent's CLI, log sink, singleton claim and Windows identity self-restart — an ordering no type can hold |
| `crates/koru-delegate-runtime/src/tests/` | the **orchestrator over fakes**: an in-memory ledger (the real `RunsDb` on `:memory:`, wrapped to record write order and fail one on demand), in-memory Run documents, a scripted session host, a capture that pins everything, a bus that records every fact, a counted sleep and a stepped clock. §7.1's four steps in order with a failure injected at each (a capture failure abandons the request naming the item and no document exists); the key bound before the host is asked; the preamble, the run line, then the objective; a busy scope warning and proceeding; every §7.5 row with tool completions coalesced and deltas never written; D29; both §7.4 fences and a takeover-boundary cancel that never terminalizes; repeated, corrected and late usage with the retry reusing its event id; commit-then-queue-then-publish-then-acknowledge and a lost publication retried under the same id; every §7.6 lookup answer including the bounded re-ask and *prior attempt unknown* never auto-resumed; a privacy sentinel through a whole run; and a dependency fence excluding every window and transport crate. The one disk exception is the working folder admission canonicalizes, under a directory named for the process |
| `apps/koru-agent/tests/delegate_worker_process.rs`, `apps/koru-agent/src/delegate_worker/tests.rs` and `crates/koru-delegate-host/src/tests/` | the **delegate worker role**: real `koru-agent.exe --worker delegate` children — the strict refusals with the role's own usage, and parent death by EOF stopping a worker cleanly while it still waits for its peers; a real Agent on a temporary root supervising the sessions, gateway and delegate workers, `POST /v1/runs` against the Codex fixture with `CODEX_FIXTURE_TURN=complete`, the run reaching *finished needing attention* with its usage recorded, the `runs.db` attempt row and the Run document agreeing on status and attempt, the stream replaying from index 0, cancel ending it, no objective or path in `koru-delegate.log`, and `koru-agent status` naming the role; and a killed worker restarted within budget with the run it was driving reading `Interrupted`, never `Failed`, once its session is gone. The colocated halves pin the strict parser, the bootstrap's redacting `Debug`, the rooted endpoint and the dependency fence |
| `apps/koru-agent/src/gateway_host/tests.rs` | the **gateway supervision adapter**, entirely through fixtures: a scripted supervision and two closures, so nothing spawns, opens a Store or touches the disk. The three fencing properties are pinned apart from each other — an owner stop that is deliberately *not* generation-fenced and ends whichever child is current; a role operation that *is* fenced and is refused once its child is replaced; and the graceful-stop adapter, whose `discover` seam is an `FnOnce` so "check A, then reach for authority again" cannot be written, and which is proved to send no shutdown at all when the published launch is not its child's. Beside them: a concurrent ensure coalesced into one answer, a restart invalidating it, a readiness failure, a stale record never answered with, an unsupervised endpoint never adopted, an exhausted budget revived once and then reported, an ensure refused inside an update hold, an aborted update resuming, and an exit nobody requested recovered from like any crash |
| `crates/koru-api-gateway/src/bootstrap/tests.rs` and `src/composition/settings/tests.rs` | the supervised refusals — a bootstrap for another channel, bridge version or root, and that the fingerprinted root is the *store* root — plus the session-bridge endpoint agreeing with the host's on a rooted and an unrooted run, the start-on-login sibling rule, and a rooted run keeping its settings under its own root. All pure: no process, no Store, no real preferences path |

Plan 10's colocated `cloud` suites freeze strict claims, transcript/framing,
client route verification, current-authority refusals, monotonic deadlines,
generation drain/fencing, strict provenance and old-writer/control gates.
They use memory only. The additional persistence proof runs explicitly with
`cargo test -p koru-store-sqlite --features disk-tests --lib cloud_restart`:
set an absolute `KORU_ROOT` ending in `target/plan10-test-root` first. It creates
one named temporary child, uses the real Store apply/flush port and reopens
the files; no running Store or default installation is discovered.

Two rules that are easy to break by accident:

- **Tests must not touch disk** — with four deliberate exceptions, each under
  a temp directory named for the process: `crates/koru-docstore`,
  `crates/koru-workspace/tests/opening.rs`, and
  `crates/koru-workspace/src/store/tests.rs` (the persisted-outbox recovery
  seam), where the thing under test *is* what happens on disk; and the
  atomic-persistence proofs for the session host's registries and the
  settings authority (UI-refactor program, rev 4 §6), where
  staged-write-and-rename *is* the thing under test — those stay minimal,
  and their ordinary policy/state tests stay pure. No test, ever, resolves a
  real preferences, local-app-data, Agent, or Store path.
  Everywhere else, only `Session::open` reaches a
  workspace directory and `koru-workspace-host`'s `App::new` is its only caller.
  `apps/koru/src/tests.rs` uses a `Harness` that bundles a document with a
  `Leaf`, since a view borrows a document rather than owning it — and a
  `Leaf` rather than one view, because Tab means different things in the two
  and a harness that skipped the dispatch would pass while the app was broken.
  It is deliberately a *bare* `Leaf` and not the container: a leaf on its own
  still believes it owns the window, so every coordinate in that file stays
  window-origin. What the container adds is proved separately, in
  `apps/koru/src/mainview/tests.rs`, by driving one click through both paths
  and comparing.
- **The theme is a process-global `AtomicBool`.** Any test that flips it must
  take the shared mutex in `apps/koru/src/tests.rs`, or it races the others
  under cargo's parallel runner.
- **The suite still lives in one file, in the manager.** It should be three, one
  per crate that owns the behaviour it drives; splitting it is the last piece of
  this refactor and is on the roadmap. Everything it covers runs
  through the public API of the crates below, which is a stronger check of the
  split than in-crate tests would be — but it is not where CLAUDE.md says tests
  belong.

Run them as described in the [contributor guide](README.md#testing-and-implementation).

### Android account adapter boundaries

OAuth Plan 2.1 exercises the same set scheduler through two fixed effect lanes.
Injected barriers hold one member before releasing its HTTP/ceremony result;
fake time drives independent retry deadlines. Tests assert that a sibling can
complete before the held effect is released, unchanged members retain their
proof and backoff, and stale selection/member incarnations cannot recreate
removed intent. The production dialogue fixture commits to an in-memory trust
repository, drops Complete, then replays the exact Begin; an early offline retry
must not replace its attestation or candidate. `enrollment::Driver` lets the real
Agent and mobile adapters compose this same controller with injected effects;
it is not a test implementation of the scheduler. The shared fixture is exported
only under runtime's existing `test-support` feature, enabled by the Agent and
mobile host dev-dependencies; production defaults do not enable it. Its directory
is the actual in-memory schema 5 implementation, while ceremony and provisioning
effects are injected and are not evidence of physical trust exchange. These checks leave physical
network, provider, vault and Apple behavior unverified.

Plan 1.8 adds pure `AccountPresentationTest` fixtures for zero-Profile accounts,
single authorized selection, stale generations, cursor gaps, live retry
backoff, terminal vault failure and logout admission. Locale tests compare every
account key, duplicate keys and format placeholders in eight resource sets.
Rust and Kotlin decode the same `account_empty_profiles.pb` fixture; the host
mapper separately proves that zero choices preserve the shared display name.
Injected Compose instrumentation tests use a plain Application so test launch
cannot initialize the real Store, account container, provider or vault. Compiling
those tests is recorded separately from connected execution, which needs a
device. A barrier fixture pauses mobile admission between enqueue and requester
registration, proving poll cannot split those effects. Shared ownership fixtures
cover retry-held envelopes, requester isolation and terminal failure without a
result. Existing QR migration/admission tests remain shared and unchanged.

`AndroidAccountAdapterTest` uses a fake dialog, coroutine clock and vault to
exercise non-consuming prompts, lost JNI acknowledgements, cancellation races,
foreground changes, bounded provider clearing and exact vault-effect replay.
`AccountCredentialVaultTest` uses injected memory storage and wrapping keys with
real AES-GCM; it checks rotation, missing-key failures and namespace isolation
without opening Android Keystore or writing a file.

The process-death evidence is split deliberately. Before remote commit,
`production_native_cancel_removes_prompt_and_refuses_late_sdk_result` in
`koru-mobile-host/src/account/tests/production.rs` proves cancellation cannot
commit a late SDK result. After remote commit but before vault acknowledgement,
`production_native_replays_lost_http_and_waits_for_platform_vault_ack` proves the
living worker retries the exact exchange and waits for durable storage; the JVM
replacement fixture proves an empty vault starts signed out with no pending SDK
continuation. After a vault write, that composed Rust fixture creates a fresh
account service, restores the credential and rotates refresh without another
SDK flow; the JVM vault fixture independently proves a new wrapper can load the
committed encrypted entry. These fixtures do not simulate Android process
killing or Google's UI. Actual three-boundary process death, Activity rotation,
Keystore failures and retained QR sync remain device checks in the
[Android README](../../apps/android/README.md).

---

## `-p <crate>` and `--workspace` do not compile the same crate

Cargo unifies features across whatever is in the build, so a crate compiled
alone and the same crate compiled inside `--workspace` can differ. It has bitten
in both directions, and each direction hides the other's bug:

- **The workspace supplies a feature the crate never asked for.**
  `koru-session-runtime`'s `agent-client-protocol-schema` turns on
  `serde_json/preserve_order`, which swaps `serde_json`'s object map from a
  sorted `BTreeMap` to an insertion-ordered `IndexMap` **for every crate in the
  build**. JSON key order therefore depends on what else was compiled. Anything
  whose *bytes* are a contract has to be independent of that:
  `koru-workspace-crypto`'s `vectors::canonical` sorts before rendering, and its
  published vectors are stable under both. Anything whose bytes are not a
  contract should compare parsed JSON rather than a literal — pinning bytes
  pins the build shape too. A binary built alone with `-p`, as a container
  image usually is, does **not** get the feature.
- **The crate needs a feature the workspace happens to enable.** `cargo test -p
  koru-account-session` does not compile: its tests need
  `--features test-support`, and only something else in a workspace build turns
  it on.

So a green `cargo test --workspace` is not evidence that a crate builds alone,
and a green per-crate run is not evidence about the bytes it produces in the
suite. When a JSON assertion fails in one and passes in the other, this is why.

**Product feature flags are the one place this is used on purpose.** Every
crate that reads a `koru_features` flag turns on `koru-features/testing` in its
own `[dev-dependencies]`, so `-p <crate>` and `--workspace` agree: under it the
default is every surface on and the environment is ignored, and a test that
means the simplified product calls `koru_features::testing::with(
Features::ALL_OFF, …)`, a **thread-local** override — it does not reach a
thread the test spawns. A dependency's feature is unified into a build only
when that build compiles dev targets, so `cargo build` and a release never see
it. See [product feature flags](FEATURE-FLAGS.md).

---

## A test that passes here and fails on CI

CI's Windows runner has four vCPUs and no GPU. A workstation has many cores and
a GPU, so a test that needs something to still be running 160 ms later, or a
child process to answer within 5 s, never misses here and misses there — and
`cargo test --workspace` stops at the first failing binary, so one such test
hides every binary after it.

`scripts/dev/ci-like.ps1` runs the suite the way the runner sees it: every test
process pinned to four logical cores (the Agents, workers and fixtures it starts
inherit that), `KORU_NO_GPU_WARMUP=1` as `ci.yml` sets it, each binary from its
package directory, a few rounds each. It reports which tests failed and how
often, and keeps only the failing runs' output.

```powershell
scripts/dev/ci-like.ps1                                   # the workspace
scripts/dev/ci-like.ps1 -Package koru-ai-server -Rounds 20
scripts/dev/ci-like.ps1 -Package koru-agent -Target '^sessions_ipc$'
```

Use it before pushing a change to anything timed, and to prove a fix for a test
that only failed on CI: the failure should reproduce under it first, then stop.
A fix that only lengthens a deadline is the last resort — what the test waits
for is usually the thing to change.

---

## Process-global state under test

The release-note generator's stdin test starts a local shell that reads through
EOF and checks the prompt, without invoking an AI service or writing a file.
On Windows it resolves PowerShell under `SystemRoot`: the bare `powershell`
command passed in a crate-only run but was not found by `cmd` in the full
workspace test environment. Keep that fixture independent of `PATH` lookup.
The Kimi Code case inspects the executable and argument vector: it receives the
same descriptor-specific instructions as Codex, as one literal `--prompt`
argument, including checkout paths containing shell operators. No test starts
an assistant or calls a model. Selection and `--yes` refusal are pure tests.
The prompt includes the complete shared writing guide and no Claude command
path, unevaluated command expression, or positional argument placeholder.

ACP diagnostic capture in `koru-session-runtime` uses one process-lifetime
test subscriber and thread-local capture with RAII restoration. A temporary
thread-local subscriber passed alone but missed static ACP callsites when other
tests first registered them concurrently without a subscriber. Dynamic interest
alone did not fix that lifetime race. The permanent test subscriber retains no
events from other threads; do not replace this with a test-thread-count override.


Session-client subscription tests exercise empty-queue condition-variable
notifications without IPC or a process. A wake only rechecks the queue predicate:
it neither means disconnection nor restarts the caller's timeout. The timed
receive uses `Condvar::wait_timeout_while`, matching the predicate loop already
used by blocking receive. Integration assertions report the unexpected receive
result so a transport close can be distinguished from a duplicate catalog event.

Agent unit tests refuse shell creation at `spawn_koru_with_stdio`, before
resolving or inspecting the sibling executable. A full workspace build can put
a real `koru.exe` beside the test binary; its absence is never a test boundary.
Cold workspace launch decisions use an injected launcher to check their exact
root, Profile selector, and stdio policy without starting a shell or Store.

The Agent test constructor also skips reopen-file discovery, so in-memory
account composition never probes the installation's recovery record. The
existing filesystem acknowledgement integration writes its own isolated record
and explicitly supplies `reopen_pending = true`; it still verifies the actual
asynchronous durable removal. A constructor guard must not silently disable the
effect that an explicit integration fixture is meant to exercise.

**The theme is a process-global `AtomicBool`.** Any test that flips it must take
the shared mutex or it races the others under cargo's parallel runner. The same
applies to the clipboard's `CLIP_LOCK`. This is why the test-thread cap in
`.cargo/config.toml` is *not* a correctness device: a suite that adds new global
state adds its own lock rather than lowering that number.

Global-input tests own no process-global state at all. Their press registrar,
key-state source, accessibility targets, and injection results are fakes. Real
RegisterHotKey, GetAsyncKeyState,
input-desktop, UI Automation, and SendInput calls belong to non-rooted
exclusive-machine acceptance. The Windows path opens no OS clipboard.

## The parent-death watcher is shared, and is not joined

Both self-hosted worker roles read one bounded bootstrap line and then keep the
same pipe open, because EOF on it is the cross-platform parent-death signal.
`koru_agent_worker::ParentPipe` owns the named thread that blocks on it;
`crates/koru-session-host` had a private copy of it and of the reader, and the
gateway role would have been the third. Nothing joins that thread — a guard
runs off the thread it guards, and a join on a thread parked in `read` is how a
shutdown wedges. It reports the *end* of the pipe rather than the first byte on
it, which the session host's private copy did not: a stray byte from a confused
parent was a shutdown request nobody sent.

## Killing a host a fixture does not own

`apps/koru-agent/tests/gateway_recovery.rs` has to kill the *serving* gateway
on both host paths, and it owns a `Child` for only one of them: the supervised
gateway is a grandchild started inside a real Agent. Enumerating processes by
command line was the obvious answer and is the wrong one — it is
platform-specific, slow, and a fixture that can find any Koru process can kill
the wrong one.

The pid is already on the wire. `instance_id` in
`crates/koru-api-gateway/src/discovery.rs` is `format!("{pid}-{millis}")`, and
both hosts drive that one composition, so the record a client discovers names
the process that answered it. The fixture parses the first component — with a
named panic, so a change to the correlation id says so rather than killing pid
0 — and one `interrupt` works for both hosts. **This is a property of the
composition, not a coincidence**; if the instance id ever stops carrying a pid,
this fixture is where to look.

## A stop is timed, not just observed

Two suites assert *how long* a stop took, and both bounds are findings rather
than round numbers.

`crates/koru-agent-worker/tests/worker_process.rs` requires a held worker whose
child already exited to finish stopping **in under five seconds**, and a
stalled graceful-stop callback to be bounded by its deadline. Reverted to the
pre-Plan-5 machine and supervisor, each of those fails at exactly 30.0 s — the
public stop's acknowledgement timeout — which is what the wedge cost before it
was corrected. A test that only asserted the final state would pass against
both.

`apps/koru-agent/tests/gateway_sync.rs` requires a gateway with an attached
window to be gone **within ten seconds** of being asked to stop. It took 90.1 s
before the shutdown path started closing its event tails, so this bound is the
regression alarm for that fix; `apps/koru-agent/tests/lifecycle.rs` prints the
same drain on every run beside its own far looser bound, so a slide shows up as
a number in the transcript rather than as a timeout minutes later.

## Worker process fixture readiness

The `koru-agent-worker` process harness starts its optional grandchild before
opening the readiness socket, then writes the token and grandchild identity as
one prepared report. The parent requires the grandchild field when that scenario
requested one. Previously, a slow Windows process spawn could exceed the socket
read deadline after the token had already arrived; the parent accepted that
incomplete report and closed the connection before the grandchild was reported.
This surfaced twice during the dev/global-Voice merge. Readiness now means the
whole fixture exists; increasing the socket timeout would only hide the race.

## M3 compiled workflow verification

The ordinary runner suite now checks strict case selection, fixture identity,
ambiguous-target refusal, content-redacted diagnostics, offscreen reading versus
pointer delivery, and bounded state waits. PageDown tests prove that an unchanged
projection does not receive another navigation key; focus waits do not double-click
while awaiting acknowledgement. The supervisor's new delivery tests hold a fake
provider until *after* timeout or cancellation has returned, then resume it and
assert zero native consequences. These are causal checks, not native UIA evidence.

The shared native crate's pure tests cover UTF-16 units (including surrogate pairs,
combining marks and bidi), accepted-prefix owned-key cleanup, and permit revocation.
The original Agent extraction proof is unchanged. A second explicitly ignored proof
uses owned EDIT windows to exercise the runner's pointer/text/chord boundary:

```sh
cargo test -p koru-windows-input --lib workflow::native -- --ignored --nocapture
```

It requires a foreground console on an exclusive Windows desktop. Ordinary tests
never run it or the compiled live cases. Ten consecutive real case passes, non-default
DPI, long fixtures, save failure, secure-desktop transitions and real screen-reader
checks remain separately recorded acceptance; a pure suite cannot establish them.

The task-rail prerequisite has colocated pure provider tests for all five queues,
shared hit geometry and activation, keyboard reachability, all eight languages,
and retirement of the document tree that previously remained exposed in Tasks mode.

### M3 repetition evidence

Runner report tests serialize into memory, including early preflight failure,
missing-target state, an unfinished reset, duplicate iterations and content
redaction. A checkpoint is progress, not a completed case. Fake UIA and a fake
clock verify that a missing later poll cannot report a previous property as
current, and that reverse page navigation waits for observed progress. Snapshot
tests pin UTF-16 output limits without splitting surrogate pairs and keep
explicit content out of Debug. Neither these tests nor fixture validation opens
a Store, reads/writes a fixture file, or sends native input.

Ten live consecutive runs of each compiled workflow remain separate evidence.
Generated fixtures and a passing pure runner suite do not establish their result.

### Shipping accessibility wrapper

`apps/koru` uses `Host::V = ManagerView`, so provider tests must exercise that
wrapper as well as MainView. The initial live M3 capture saw only the window and
three caption commands: seven missing ManagerView overrides inherited empty/no-op
View defaults, although MainView tests passed. The colocated manager accessibility
suite now covers seating, the real body subtree, caption geometry, focus/actions,
revision changes, retirement draining and the already-forwarded durability state.
It also pins the popout arm's existing unsupported behavior. Action forwarding
uses the View implementation so normal post-gesture request draining runs.
All fixture state is in memory; external UIA capture remains separate live evidence.

### M3 live-boundary regressions

Runner preparation tests model cold paragraphs with no text until page navigation
lays them out. They permit only focus and page keys, refuse wrong identity and a
clipped entry click before delivery, preserve cancellation, and reject reordered
paragraphs before manifest publication. Visibility tests deliberately keep raw
UIA IsOffscreen false for an unlaid paragraph and use actual body intersection
separately from a safe click midpoint. Case-order checks require reading text
after reveal and before editing, including on a fresh reopen.

Editor projection tests move a nonzero pane origin and verify row, text-run,
checkbox and pill bounds translate without reshaping. A MainView client keeps its
existing tree through sidebar and split-pane changes and checks updated body bounds
against the shared drawing clip and paragraph coordinates against hit testing.
These checks exercise the composition absent from the original zero-origin
fixtures. They remain in-memory checks; live capture and native input results must
still be recorded separately.
