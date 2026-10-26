# Contributing to Koru

Thank you for wanting to help. This page covers the license a contribution
comes in under, and then the short list of things that keep a change easy to
review.

The [contributor guides](guide/contributing/README.md) cover building, testing,
Rust, and library behavior. The [user guides](guide/user/README.md) describe
the behavior a change must preserve. Both are part of this repository and need
no access to the private documentation submodule.

## License

Koru is licensed under the [Mozilla Public License 2.0](LICENSE). By
contributing, you agree that your contribution is licensed under MPL-2.0 too.
Koru does not ask you to sign a contributor license agreement, and commits need
no sign-off line. You keep the copyright in your contribution.

Sending a change is your statement that you wrote it, or otherwise have the
right to submit it under MPL-2.0. **Code written with an AI assistant** is held
to the same standard: you reviewed the change, and it does not reproduce code
under an incompatible license.

Every first-party source file starts with an SPDX identifier:

```rust
// SPDX-License-Identifier: MPL-2.0
```

You do not have to add it by hand. After adding files, run:

```sh
python scripts/checks/spdx-headers.py           # add the header where it is missing
python scripts/checks/spdx-headers.py --check   # what CI runs
```

The script knows what must *not* be headed: third-party and generated code, test
fixtures, and SQL migrations (sqlx checksums them, so editing one breaks every
existing database). If you copy in code from another project, keep its
original license and copyright notice, do not add an MPL header to it, and say
so in the pull request. [licensing/](licensing/README.md) has the
details.

The name "Koru", the logo and the app icons are trademarks and are not covered
by MPL-2.0; see [TRADEMARKS.md](TRADEMARKS.md).

## Before you open a pull request

- **Read [AGENTS.md](AGENTS.md).** It is the orientation for humans and agents
  alike, and it links the page you need for whichever area you are touching.
- **Run the suite:** `./koru.sh test` (or `koru.bat test` on Windows). It is
  `cargo test --workspace` and the suite is the spec.
- **Format:** `cargo fmt`.
- **Repository checks:** `python -m unittest discover -s scripts/checks -p "test_*.py"`.
  CI also checks SPDX headers and scans a clean source export for publication
  mistakes and broken links. The Windows suite runs on the pinned Rust
  toolchain and the minimum version declared in `Cargo.toml`.
- **Added a source file?** It starts with the SPDX header;
  `python scripts/checks/spdx-headers.py` adds it and `--check` lists what is missing.
- **Adding third-party code?** Do not commit what can be fetched: pin it in
  `third-party/fetch.manifest` ([third-party/](third-party/README.md)).
- **Keep the docs with the code.** Update the public guides in
  [guide/](guide/README.md), doc comments, and generated
  references ([reference/](reference/README.md)) your change touches. The design
  docs and ADRs are internal
  ([GOVERNANCE.md](GOVERNANCE.md#proposals-and-decisions)): say in the pull
  request what a page or decision should record, and a maintainer records it.
- **UI changes** reuse the shared components in `crates/koru-ui` before adding
  new ones; a new shared component starts as a proposal (below).
- **One change per pull request**, with a description that says what changed
  and why. Small pull requests are reviewed sooner.

## Koru Cloud: running without one, or with your own

Sign-in, accounts and Cloud sync talk to a Koru Cloud server, and which one is
build configuration: official builds bake in Koru's; the source does not
contain it. A build without one works in full on its own device and says so
honestly: the first-run welcome offers only *Continue without an account*,
and wherever sign-in, an account or Cloud sync would be offered it says
*Koru Cloud isn't set up in this build*. That is not an error and nothing
retries it; Koru makes no network call to a Cloud it does not have.

`koru.bat run`, `run-all` and `root` (and `./koru.sh run`, `run-all`) read the
Cloud from `.env.production` at the repository root. With no such file they
start with Koru Cloud not configured and print one line saying so. With the file
present, its values are used, a shell value that disagrees with it is announced,
and an empty file is the explicit choice of no Cloud.

To point a build at a Koru Cloud, give it the Cloud's origin and token issuer,
in the file or in the environment of the process that runs Koru (a release
build bakes them in from `.env.production`):

```sh
KORU_PUBLIC_API_ORIGIN=https://cloud.example.org
KORU_PUBLIC_API_ISSUER=https://cloud.example.org
```

[reference/ENV.md](reference/ENV.md) describes both, and the
optional `KORU_PUBLIC_API_CREDENTIAL_ROLLOUT`.

## Google Docs import: supplying your own Google registration

The Google Docs importer signs in with a Google OAuth client. Official Koru
builds bake in Koru's own registration; the source does not contain it. A build
without one still lists *Google Docs* in the import menus, dimmed, with the
reason beside it ("Not set up in this build"), and nothing else changes.

To use the importer in your own build, register a client of your own:

1. In the [Google Cloud console](https://console.cloud.google.com/), create or
   pick a project and enable the **Google Drive API** and the **Google Picker
   API**.
2. Configure the OAuth consent screen and add the
   `https://www.googleapis.com/auth/drive.file` scope. While the app is in
   testing, add your own account as a test user.
3. Under *APIs & Services → Credentials*, create an **OAuth client ID** of type
   **Desktop app**. The importer needs exactly that type: it redirects to a
   loopback address on a random port, which Google allows only for desktop
   clients.
4. Give the build the client's id and secret, either in the environment of the
   process that runs Koru or of the `cargo build` that compiles it (a release
   build bakes them in):

   ```sh
   export KORU_GOOGLE_DOCS_CLIENT_ID="1234-abc.apps.googleusercontent.com"
   export KORU_GOOGLE_DOCS_CLIENT_SECRET="GOCSPX-..."
   ```

   or as two lines in the `.env.production` your launcher reads.

**The secret is not confidential, and that is expected.** Google treats a
desktop client's secret as public, because anything shipped in a binary can be
extracted. The importer authenticates with PKCE, checks the OAuth `state`, and
receives the redirect on `127.0.0.1` only. Do not reuse a client that is also
used as a confidential *web* client. [reference/ENV.md](reference/ENV.md)
describes both variables.

## Reporting a security issue

Please do not open a public issue for a vulnerability. Email
**security@koru.do** with the details, and we will reply there.
[SECURITY.md](SECURITY.md) says what to include, what is in scope (the hosted
Koru Cloud service too) and how quickly we respond.

## Proposals, governance and conduct

A change to a shared contract or component starts as a public proposal, and a
maintainer records the accepted decision: [GOVERNANCE.md](GOVERNANCE.md)
describes how, who the maintainers are, and which platforms are maintained.
Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).

## Release builds

Official publication requires a clean Git checkout, including untracked files
and submodule changes. Commit release descriptors before retrying if the tool
generates notes. The publisher checks the checkout before building and again
before signing; it refuses if the source changes or `HEAD` moves. The packaged
notice names that commit, which must be available in the public repository.
Local experiments use `dist-local`, which permits uncommitted changes.

Rust is pinned in `rust-toolchain.toml`, and packaging uses the checked-in
`Cargo.lock` with `--locked`. CI packages a build without private configuration
and starts its executable with `--help`; interactive application verification
is still required before a release. Android tests are not in CI yet.
