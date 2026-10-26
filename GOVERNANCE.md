# Governance and support

Who decides what in Koru, which platforms are maintained, how issues and
proposals are handled, and where the open-source project ends and the hosted
service begins.

## Maintainers

| Maintainer | GitHub | Responsible for |
| --- | --- | --- |
| Steven Liechti | [@sliechti](https://github.com/sliechti) | Everything: review and merge, releases, security, the hosted service |

Koru is developed by Koru Works, Inc. Maintainers review and merge pull
requests, approve proposals, publish releases and receive security reports.
They are also the code owners of the paths that control what CI runs and what a
release contains ([`.github/CODEOWNERS`](.github/CODEOWNERS)). This table
grows as people join; a new maintainer is added by a pull request to this page
that the existing maintainers approve.

## Platforms

| Platform | Status |
| --- | --- |
| **Windows** (x64) | **Supported.** Official builds on every channel; CI builds and tests every change on Windows, and packages every merge |
| **Android** | **Maintained.** Built with Gradle from `apps/android`. It uses Google Play services, which are proprietary |
| **macOS** (Apple silicon and Intel) | **Held.** The source builds on a Mac, and it now links upstream's text-to-speech-free voice runtime, with no eSpeak NG (GPL-3.0), as Windows does. Official macOS releases wait for that to be verified on a Mac ([licensing](licensing/THIRD-PARTY.md#espeak-ng-gpl-30-in-three-shipped-binaries)) |
| **iOS** | **Held.** The source is here, with its license notices; the first release waits for it to be built and verified on a Mac ([licensing](licensing/README.md#android)) |
| Linux | **Not a Koru target.** CI runs only platform-independent checks there (the repository's own checks, formatting, licenses and advisories). Nothing claims the app builds or runs on Linux, and there are no packages |

**Apple platforms have limits we cannot remove.** Building for macOS or iOS
needs a Mac with Xcode. Running a build on an iPhone needs an Apple developer
account, and anything distributed needs Apple's signing and, for macOS,
notarization; official builds are signed with Koru's identity, which is not
part of this repository. Public CI has no macOS or iOS runner, so a change to
Apple-only code is checked by a maintainer on a Mac before it is merged.

## Issues and triage

- **Bugs and feature requests** are GitHub issues, opened from the templates.
- A maintainer triages new issues, normally within a week: confirms or asks
  for detail, labels it, and says whether it is planned, open for a
  contribution, or declined (with the reason).
- **Questions and support** go to GitHub Discussions when they are enabled, or
  an issue until then.
- **Security problems are never public issues.** Report them as
  [SECURITY.md](SECURITY.md) describes.
- Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).

## Proposals and decisions

A change to a shared contract (wire formats, the Store's data model, the
public APIs), to a KoruX component, or to how Koru is built starts as a
**proposal**:

1. Open an issue (the "Proposal" template) or a discussion, saying what you
   want to change, why, and what you considered instead.
2. Maintainers and anyone interested discuss it there, in public.
3. A maintainer records the outcome on the issue: accepted, accepted with
   changes, or declined with the reason.
4. **A maintainer records an accepted decision as an ADR** in Koru's internal
   documentation, linking the issue. ADRs record *why*; the code and the
   reference docs say *what*.

**What is public, and what is not.** The code, the generated references in
[reference/](reference/README.md), the licensing audit in
[licensing/](licensing/README.md), [AGENTS.md](AGENTS.md), this page,
[CONTRIBUTING.md](CONTRIBUTING.md), and every issue and pull request are
public. The design documents and the ADRs are internal: they live in a private
documentation repository, which a team checkout mounts at `docs/`, so links
into `docs/` resolve only there. An outside contributor never needs them: the
discussion happens on the issue, and a maintainer carries its outcome into the
internal docs. Small fixes need no proposal; open a pull request.

## Security

Security reports go to **security@koru.do** ([SECURITY.md](SECURITY.md)).
The maintainers above receive them and own the response for this repository
and for the hosted service alike.

## The hosted service

**Koru Cloud** is the optional service an official build can sign into. It
provides:

- accounts and sign-in (including with Google and Apple);
- Cloud sync and storage of your documents between your devices;
- the public API at `api.koru.do`;
- the metrics service that receives opt-in usage telemetry, which any build,
  official or from source, sends only after you agree to it on that device
  ([README](README.md#usage-telemetry-is-opt-in) says how to turn it off);
- the release and update downloads official builds install from.

Koru also operates relay servers for peer-to-peer connections. The relay's
code (`apps/koru-relay`) and the recipe for its container image
(`apps/koru-relay/Dockerfile`) are in this repository; only its deployment is
ours.

**The service's server code is closed source** and is not in this
repository. The client side is here: everything the apps link, including the
code that talks to Koru Cloud and the contracts both sides speak. Koru works
in full without it: a build from this repository makes no call to a Cloud it is
not configured for, and peer-to-peer sync between your own devices needs no
Koru account.
[CONTRIBUTING.md](CONTRIBUTING.md) explains how to run without a Cloud or
point a build at your own.

Problems with the hosted service are reported here too: security issues as
[SECURITY.md](SECURITY.md) says, everything else as an issue.

## Licensing

Koru is [MPL-2.0](LICENSE), and contributions come in under it
([CONTRIBUTING.md](CONTRIBUTING.md)). The name and logo are trademarks
([TRADEMARKS.md](TRADEMARKS.md)).
