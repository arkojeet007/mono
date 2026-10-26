<p align="center">
  <a href="https://koru.do"><img src="assets/logo.png" alt="Koru logo" height="80"></a>
</p>
<p align="center"><strong>Your private workspace for thinking, writing and AI.</strong></p>
<p align="center">
  <a href="https://koru.do"><img src="https://img.shields.io/badge/%E2%9C%A8-Website-0a0a0a.svg?style=flat&colorA=0a0a0a" alt="Website"></a>
  <a href="https://koru.do/#download"><img src="https://img.shields.io/badge/%E2%9C%A8-Download-0a0a0a.svg?style=flat&colorA=0a0a0a" alt="Download Koru"></a>
  <a href="https://koru.do/en/roadmap"><img src="https://img.shields.io/badge/%E2%9C%A8-Roadmap-0a0a0a.svg?style=flat&colorA=0a0a0a" alt="Roadmap"></a>
  <a href="guide/user/USERGUIDE.md"><img src="https://img.shields.io/badge/%E2%9C%A8-User%20guide-0a0a0a.svg?style=flat&colorA=0a0a0a" alt="User guide"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MPL--2.0-0a0a0a.svg?style=flat&colorA=0a0a0a" alt="License: MPL-2.0"></a>
  <a href="https://x.com/liechti_dev"><img src="https://img.shields.io/badge/follow-%40liechti__dev-0a0a0a.svg?style=flat&logo=x&logoColor=white&colorA=0a0a0a" alt="Follow @liechti_dev on X"></a>
</p>

<p align="center">
  <img src=".github/readme/koru-workspace.webp" alt="Koru on Windows: a travel-planning note with a photo and an itinerary, next to a Claude Code agent tab" width="100%">
</p>

# Koru is a private, local-first AI workspace

Koru is an open-source, local-first workspace for thinking, writing and working
with AI. Documents, files and terminal sessions, including your shells and AI
coding agents, open as tabs in one native window. Your work stays on your own
devices, works offline, and syncs directly between them, with no cloud server
holding your data.

## What can you do with Koru?

- **Write and organize**: documents with headings, tables, checklists, code,
  images and attachments, nested in a tree, with full-text search, `#tags` and
  named versions you can compare.
- **Work with AI on your terms**: open Claude Code, Codex, Gemini and other
  coding agents as tabs beside your documents, and let any MCP client, including
  a local model, read and edit your workspace through Koru's built-in MCP server.
- **Bring your notes with you**: import from Evernote (`.enex`), Notion,
  OneNote, Google Docs, Markdown and TextBundle, and export to Markdown or
  TextBundle at any time.
- **Speak instead of type**: Koru Voice dictates on your own device; your audio
  never leaves it.
- **Keep it yours**: free, no account required, and open source under the
  [MPL-2.0](LICENSE).

## What is Koru an alternative to?

Koru is a private, local-first alternative to cloud note-taking and document
apps. Your documents, files and AI agents stay on your own devices, work
offline, and sync directly between them, with nothing stored on someone else's
server.

- **[Evernote alternative](https://koru.do/en/alternatives/evernote)**: your
  notes live on your own devices instead of Evernote's cloud, and they're fully
  available offline.
- **[OneNote alternative](https://koru.do/en/alternatives/onenote)**: you don't
  need a Microsoft account or OneDrive, because devices sync peer-to-peer.
- **[Notion alternative](https://koru.do/en/alternatives/notion)**: it's built
  to work offline first, and it syncs device to device instead of through a
  cloud server.
- **Google Docs alternative**: your writing stays private on your devices, with
  no Google account and no cloud storage.

## Does Koru work offline?

Yes. Koru stores every workspace on your own devices, so you can read and write
without an internet connection (an AI agent still needs whatever connection its
own model needs). When your devices reconnect, they sync directly with each
other, peer-to-peer. No cloud server stores your data, and sync traffic is
end-to-end encrypted, including when a relay has to forward it between networks.

## How does Koru sync without a server?

Koru stores each workspace as a [Loro](https://loro.dev/) snapshot. Loro is a
CRDT (conflict-free replicated data type), so two devices editing the same
workspace always end up in the same state without a server to coordinate them.
If you only use one device, Koru never exchanges an operation, so the sync
option costs you nothing.

## How is a Koru workspace structured?

A Koru workspace is a tree of typed nodes: documents, tasks, contacts and
images. Their attributes are stored as structured data rather than plain text,
and each view, such as a list, a table or a document, is a projection over that
one model.

## What is Koru built with?

The Koru desktop app is written in Rust. It uses
[parley](https://github.com/linebender/parley) for text layout,
[vello](https://github.com/linebender/vello) for GPU rendering, winit for
windowing, and [iroh](https://iroh.computer/) for device-to-device sync. The iOS
and Android apps are native Swift and Kotlin, built on the same contracts.

## Status

Pre-1.0 and moving quickly. The first public release shows **documents, files,
and terminal sessions for your shells and AI coding agents**. More is built than
is shown: tasks, the Koru AI chat, the Agents and Artifacts views, canvases,
reminders and Koru Cloud sign-in sit behind product flags that are off unless a
build turns them on
([product feature flags](guide/contributing/FEATURE-FLAGS.md)). The
[user guide](guide/user/USERGUIDE.md) describes the available behavior and
experimental features.

| Platform | Status |
| --- | --- |
| **Windows** (x86-64) | Supported. Builds, runs and packages from this repository |
| **macOS** | Builds and runs from source for development. Public macOS builds are held until a Mac verifies that they link the voice runtime's text-to-speech-free archive, which the tree now uses ([licensing/THIRD-PARTY.md](licensing/THIRD-PARTY.md#espeak-ng-gpl-30-in-three-shipped-binaries)) |
| **Android** | The native app builds with Gradle and is in active development; see [Android setup and verification](apps/android/README.md) |
| **iOS** | Held. The project in `apps/ios` is not released yet; the first release waits for it to be built and verified on a Mac |
| **Linux** (x86-64) | Preview. A [nightly build](https://github.com/koruworks/mono/releases/tag/nightly) is published as a tarball that updates itself; voice input and Koru Cloud are not available on Linux yet |

## Build and run from a fresh clone

You need:

- **Rust**, from [rustup](https://rustup.rs). The checked-in
  `rust-toolchain.toml` selects Rust 1.99.0 for development and release builds;
  rustup installs it automatically. The minimum supported version is 1.95,
  declared in `Cargo.toml` and tested in CI. Windows also needs the MSVC build
  tools.
- **sccache**, which is not optional: `.cargo/config.toml` sets it as the
  compiler wrapper. `winget install Mozilla.sccache`, or `cargo install sccache`.
- **A GPU with DX12, Vulkan or Metal.** vello renders with compute shaders and
  has no software fallback.
- **Python 3**, for the repository's checks under `scripts/checks/`.
- **curl**, which every fetch script under `scripts/setup/` downloads with.
  macOS and Windows 10+ ship it.

Then, on Windows:

```bat
git clone https://github.com/koruworks/mono.git koru
cd koru
koru.bat setup   REM sets up whatever is not set up yet, and lists what it could not
koru.bat run
```

`./koru.sh setup` and `./koru.sh run` are the same on macOS.

`setup` installs sccache; sets its cache directory, cache size and the cargo job
count for this machine (user environment variables on Windows, a marked block in
your shell profile elsewhere); and fetches fonts and third-party code. What is
already set up it leaves alone, and whatever it cannot do it lists with the
command that does it. Run it again at any time: it does only what is still
missing, and retries what failed. `setup --check` reports without changing
anything, including when combined with the optional flags below.

Two setup options are available separately:

- **`koru.bat setup --defender-exclusions`**: recommended for faster builds
  in a trusted local checkout after reviewing the trade-off. Windows asks for
  elevation. It disables Defender scanning for the repository, worktrees,
  Cargo dependency sources, and compiler cache. Leave it off when reviewing
  untrusted contributions. `scripts/setup/defender-exclusions.ps1 -Remove`
  reverses the exclusions.
- **`koru.bat setup --agent-skills`** (or `./koru.sh setup --agent-skills`):
  install coding-agent skills and the Git hook that installs them in new
  worktrees. This requires Node.js and runs the skills CLI. Its anonymous
  install telemetry can be disabled with `DISABLE_TELEMETRY=1`.

Ordinary build, test, and run commands do not install agent tooling or change
Git hooks. Both optional flags can be combined on Windows.

`run` downloads the bundled fonts on first use (`koru.bat fonts` does just that
step; the faces are pinned by hash in `scripts/setup/fonts.manifest`), compiles, and
launches. The first compile takes several minutes and everything after it is
incremental. **Fetch the fonts before running the tests on a fresh clone**, or
the typography tests fail.

Every launcher verb also fetches three crates Koru builds from patched source
(`onenote_parser`, `sherpa-onnx`, `sherpa-onnx-sys`) from crates.io and applies
Koru's patches, which are all of them this repository holds
([third-party/](third-party/README.md)). **To run `cargo` directly on a fresh
clone, run `scripts\setup\fetch-third-party.ps1` (or `bash
scripts/setup/fetch-third-party.sh`) once first**. Without them cargo stops with
`patch location ... third-party/crates/sherpa-onnx does not contain packages
matching`.

**Coding agents get Koru's skills from
[koruworks/skills](https://github.com/koruworks/skills)** (the document-driven
workflow: draft, review, plan, execute, hand off), installed with `npx skills
add` into `.agents/skills/`, which `.claude/skills` links to; none of it is
committed. `setup --agent-skills` installs them and sets the Git hook that
installs them in every `git worktree add` after it. It needs Node.js for `npx`,
and nothing else needs the skills: without Node the build is unaffected. To take
a newer set, run `sh scripts/setup/setup-skills.sh --update`.

**Koru Cloud is off in a build from source, and that is expected.** With no
`.env.production` at the repository root, `run` starts with Koru Cloud not
configured and says so in one line; the app works in full on its own device and
syncs directly between your devices. To point a build at a Koru Cloud of your
own, see [CONTRIBUTING.md](CONTRIBUTING.md#koru-cloud-running-without-one-or-with-your-own).

### Usage telemetry is opt-in

A build from source has the same telemetry destination as an official one,
Koru's metrics service at `https://m.koru.do`, and the same rule: nothing is
sent until you agree. Koru asks once on each device. What it sends is which
features are used, how well the app runs, and basic facts about the device such
as its operating system and screen size; never your documents, their names or
anything you type. The *Share usage data* switch in Settings changes the answer
at any time. `KORU_TELEMETRY_ENABLED=0` removes the destination altogether, and
`KORU_TELEMETRY_BASE_URL` points it at a service of your own
([reference/ENV.md](reference/ENV.md)).

### Voice and agents need CMake and libclang

`koru.bat run` launches the app alone. Koru Voice, the agent host,
`koru.bat run-all` and packaging also link [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx)
for speech recognition, and whisper.cpp, which is compiled from source:

```bat
winget install Kitware.CMake LLVM.LLVM     REM CMake, and libclang for the bindings
koru.bat run-all
```

The launcher downloads sherpa-onnx's prebuilt archive the first time (about
117 MB) into `third-party\sherpa-onnx\`, and refuses it unless it matches the
hash Koru pins; a new worktree copies it from the main checkout instead. It is
upstream's build **without text-to-speech**: the ordinary one links eSpeak NG,
which is GPL-3.0, and Koru does not use it. Why, and what ships:
[licensing/](licensing/README.md).

### Tests, packages and phones

```bat
koru.bat test                  REM cargo test --workspace, the whole suite
koru.bat dist-local beta       REM an unsigned local package under dist\ (needs the voice archive)
```

Android needs JDK 17 and Android SDK 37. The Gradle wrapper is fetched, not
committed, so on a fresh clone run `scripts\setup\fetch-third-party.ps1` (or
`bash scripts/setup/fetch-third-party.sh`) once, then `cd apps/android` and
`./gradlew testDebugUnitTest assembleDebug`. See the
[contributor guides](guide/contributing/README.md) for testing and implementation
guidance, and the [Android README](apps/android/README.md) for platform setup.

The launchers are thin: what they run lives under `scripts/`, in four folders.
`setup/` fetches and builds what a fresh clone lacks (fonts, dictionaries, the
third-party code, the voice runtime, the skills) and checks the machine;
`dev/` starts, stops, captures and probes the app while you work on it;
`checks/` holds the repository's own checks (links, the publication scan, SPDX
headers), which CI runs too; and `icons/` regenerates the app and
mobile icons from the logo.

## Where to go next

The user and contributor guides live in [guide/](guide/README.md), alongside
the source, and are available in every clone. Internal plans, ADRs, company
notes, and hosted-service operations remain in the private `docs/` submodule.

| If you want to know | Read |
| --- | --- |
| How to work on the code — for people and coding agents alike | [AGENTS.md](AGENTS.md) |
| Where to start with the public documentation | [guide/](guide/README.md) |
| How to contribute | [CONTRIBUTING.md](CONTRIBUTING.md) |
| How to use it | [guide/user/USERGUIDE.md](guide/user/USERGUIDE.md) |
| Building, testing, Rust, and library behavior | [Contributor guides](guide/contributing/README.md) |
| The local API and wire schemas | [Generated references](reference/README.md) |
| The document model and MCP server | [MCP model and tools](crates/koru-mcp-server/MCP.md) |
| Environment variables, events, and shortcuts | [reference/](reference/README.md) |
| Licensing and third-party attribution | [licensing/](licensing/README.md) |

## Koru Cloud is not in this repository

Koru Cloud is the hosted service official builds can sign in to: accounts and
sync through the cloud. It is run by Koru
Works, Inc., and its server code, infrastructure and credentials are not part
of this repository. Everything the apps link is here, including the client side
of Cloud and the wire contracts it speaks (`contracts/`), so a build from this
source can talk to any server that implements them — or to none.

Official builds are this source at a tag, built with configuration only we
hold: the Cloud origin, the update-signing key and the Google Docs importer's
registration. A build without them turns those features off and says so.

## Share with the community

If Koru is useful to you, a post about it really helps a small, independent
project get found. Follow the build on X: [@liechti_dev](https://x.com/liechti_dev).

[![Share on Reddit](https://img.shields.io/badge/share%20on-reddit-FF4500?logo=reddit&logoColor=white)](https://www.reddit.com/submit?url=https%3A%2F%2Fkoru.do&title=Koru%3A%20a%20private%2C%20open-source%2C%20local-first%20AI%20workspace)
[![Share on Hacker News](https://img.shields.io/badge/share%20on-hacker%20news-F0652F?logo=ycombinator&logoColor=white)](https://news.ycombinator.com/submitlink?u=https%3A%2F%2Fkoru.do&t=Koru%3A%20a%20private%2C%20open-source%2C%20local-first%20AI%20workspace)
[![Share on X](https://img.shields.io/badge/share%20on-x-000000?logo=x&logoColor=white)](https://x.com/intent/post?url=https%3A%2F%2Fkoru.do&text=Koru%3A%20a%20private%2C%20open-source%2C%20local-first%20AI%20workspace&via=liechti_dev)
[![Share on Bluesky](https://img.shields.io/badge/share%20on-bluesky-0285FF?logo=bluesky&logoColor=white)](https://bsky.app/intent/compose?text=Koru%3A%20a%20private%2C%20open-source%2C%20local-first%20AI%20workspace%20https%3A%2F%2Fkoru.do)
[![Share on LinkedIn](https://img.shields.io/badge/share%20on-linkedin-0A66C2?logo=linkedin&logoColor=white)](https://www.linkedin.com/sharing/share-offsite/?url=https%3A%2F%2Fkoru.do)
[![Share on Facebook](https://img.shields.io/badge/share%20on-facebook-1877F2?logo=facebook&logoColor=white)](https://www.facebook.com/sharer/sharer.php?u=https%3A%2F%2Fkoru.do)

## License

Koru is free software under the [Mozilla Public License 2.0](LICENSE). Change a
Koru file and distribute it, and that file stays open; add your own files beside
ours under whatever license you like. Contributions come in under MPL-2.0 too
([CONTRIBUTING.md](CONTRIBUTING.md)). Third-party components and what they
require are listed in [licensing/](licensing/README.md).

The Koru name, logo and icons are trademarks of Koru Works, Inc. and are not
covered by the license; see [TRADEMARKS.md](TRADEMARKS.md).
