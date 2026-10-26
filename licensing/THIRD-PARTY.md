# Third-party code, assets and attribution — audit

*Audited 2026-10-11, and updated 2026-10-14, from `cargo metadata` over the
resolved graph, the fetch scripts, the Gradle/SwiftPM manifests, the upstream
license files at the exact revisions we build, and the shipped binaries of the
2026-10-09 nightly.* What is listed here is what a **distributed build** owes
the people whose work it contains. Open items are marked **⚑** and collected
at the end, and settled ones are marked ✅.

## Summary

- **Every package now carries its notices.** `LICENSE`, `NOTICE` and a
  generated `THIRD-PARTY-NOTICES.txt` are required release files (see
  [README.md § In a release](README.md#in-a-release)). Before this, no package
  carried them.
- **Every crate declares a license**, and the only copyleft crates are MPL-2.0,
  the same license as Koru.
- **The one serious finding, fixed: GPL-3.0 code was shipping.** The voice
  runtime statically linked eSpeak NG into three of our binaries. Every
  platform now links upstream sherpa-onnx's own **text-to-speech-free**
  archive, so no build contains it. Verified on Windows; a Mac link test is
  still owed. See [below](#espeak-ng-gpl-30-in-three-shipped-binaries).

## Rust crates

The shipped binaries reach **647 third-party crates** on Windows, 656 on macOS
(Apple silicon; 658 on Intel) and 496 in the iOS app, through normal
dependencies (2026-10-14). Proc-macros are excluded because they run in the
compiler and none of their code is in a binary. No crate comes from git; all
come from crates.io, `onenote_parser`, `sherpa-onnx`, `sherpa-onnx-sys` and
`merman-render` with Koru's patches applied
([third-party/](../third-party/README.md)).

| License (as declared) | What a binary distribution owes | Handled by |
| --- | --- | --- |
| MIT and/or Apache-2.0 (all spellings, incl. dual with Zlib/BSD/ISC/0BSD/CC0/LLVM-exception) | MIT: the copyright and permission notice. Apache-2.0: the license text, plus the crate's `NOTICE` file if it has one | the crate's own files, copied verbatim ✅ |
| Unicode-3.0 (ICU4X), BSD-2/3-Clause, ISC, CDLA-Permissive-2.0 (`webpki-roots`) | Notice and license text | same ✅ |
| **MPL-2.0 / MPL-2.0+** (`spellbook`, `nucleo`, `cssparser`, `selectors`, `attohttpc`, `im`, …, `onenote_parser`) | Tell recipients where the source is (MPL §3.2(a)) | every crate line in the file names its crates.io page or repository ✅ |
| Unlicense, Zlib, BSL-1.0, CC0 | Nothing for binaries | listed anyway ✅ |

**Some crates ship no license file in their package** — 35 on Windows, 61
on macOS and 27 on iOS, mostly the `objc2-*`, `iroh-*` and `accesskit*`
families. ✅ **Every one, on every platform, now has a record** in
`crates/koru-release/licences/crates/` (68 records; Windows and Android
2026-10-13, macOS and iOS 2026-10-14, and `selectors` 0.37.0 for the graph
`koru-diagram` brings, ahead of anything shipping it, 2026-10-19): 57 carry
the license file from the crate's repository at the published revision (or,
where that revision named its license without the text, the text upstream
added later, noted in the record), and 11 whose upstream has no license file
at all carry the standard text under a recorded judgment, with a request
drafted in
[UPSTREAM-ISSUES.md](UPSTREAM-ISSUES.md). A shipped crate with neither a file
nor a record refuses the release on every platform
([README.md](README.md#a-crate-without-a-license-file)), and `koru-release
notices macos` / `notices ios` run that check from any machine.

## Native code inside our binaries

`cargo metadata` cannot see these, so they are kept by hand in
`crates/koru-release/licences/bundled/`, each with its upstream text at the
revision we build.

| Component | License | In |
| --- | --- | --- |
| whisper.cpp / ggml (vendored by `whisper-rs-sys` 0.15.0) | MIT, © The ggml authors | `koru-voice-worker` |
| sherpa-onnx 1.13.6 | Apache-2.0 | `koru-voice-worker` |
| ONNX Runtime 1.27.1 (static), with its own ThirdPartyNotices | MIT, © Microsoft | `koru-voice-worker` |
| kaldi-native-fbank 1.22.3, kaldi-decoder 0.3.0, OpenFst 1.8.5, simple-sentencepiece 0.7 | Apache-2.0 | `koru-voice-worker` |
| KISS FFT | BSD-3-Clause | `koru-voice-worker` |
| piper-phonemize | MIT | **Not linked on any platform** (2026-10-14) |
| eSpeak NG + ucd-tools | GPL-3.0-or-later | **Not linked on any platform**: gone from Windows 2026-10-11, from macOS 2026-10-14 (a Mac link test is owed, ⚑1) |
| SQLite (`rusqlite` `bundled`) | Public domain | nothing owed |

The speech runtimes are linked into the voice worker only. `koru-agent` and
`koru-com-notification` used to link them too, because `koru-agent` named the
worker as a library for its handshake types. Those types now live in
`koru-voice`, so neither links an inference runtime or `cpal`
(docs/features/linux/TECHNICAL.final.md, D3). The eSpeak NG section below
records the earlier state.

### eSpeak NG: GPL-3.0 in three shipped binaries

`sherpa-onnx-sys` (static mode, our default per
`docs/features/voice/RUNTIME_ARTIFACTS.md`) links thirteen archives, including
`espeak-ng`, `ucd` and `piper_phonemize`. Those exist for sherpa's
**text-to-speech**, which Koru does not use. We only use speech recognition.
The linker keeps them anyway. The 2026-10-09 nightly's `koru-voice-worker.exe`,
`koru-agent.exe` and `koru-com-notification.exe` each contain about 65 eSpeak
strings. `koru-agent` and `koru-com-notification` carry it because they link
the voice worker in-process.

**Why it matters.** GPL-3.0 code inside a binary makes that binary, as
distributed, a GPL-3.0 work. We must then license the whole binary under
GPL-3.0 and offer its complete source. That is *possible*: everything else in
those binaries is MPL-2.0 (MPL-2.0 is GPL-compatible) or permissive. It is not
what we want, though. It would make Koru's main background process GPL, and it
breaks the moment a proprietary package is linked (none is, and the release
tool refuses one).

**The fix: upstream's own TTS-free archives, on every platform (2026-10-14,
[ADR-460](../docs/decisions/ADR-460-upstreamnotts.md)).** Windows was first
fixed on 2026-10-11 with an archive we built ourselves with
`SHERPA_ONNX_ENABLE_TTS=OFF`. Upstream turns out to publish the same thing,
static and TTS-free, for every platform, so Koru links those and builds nothing:

| Target | Archive (sherpa-onnx v1.13.6 release) | SHA-256 |
| --- | --- | --- |
| Windows x64 | `sherpa-onnx-v1.13.6-win-x64-static-MT-Release-no-tts-lib.tar.bz2` | `6c746e0f…` |
| macOS arm64 | `sherpa-onnx-v1.13.6-osx-arm64-static-no-tts-lib.tar.bz2` | `5df455ff…` |
| macOS x64 | `sherpa-onnx-v1.13.6-osx-x64-static-no-tts-lib.tar.bz2` | `adcc1de3…` |

- **What is in them.** None has `espeak-ng`, `ucd` or `piper_phonemize`, and
  none contains eSpeak NG's code: a byte search of every library for its API
  (`espeak_ng_Initialize`, `espeak_Synth`), its phoneme tables (`phontab`,
  `phonindex`) and ucd (`ucd_lookup_category`) finds nothing, while the same
  search finds them in upstream's ordinary macOS archive, in `libespeak-ng.a`
  *and* inside `libsherpa-onnx-core.a`. The Windows archive has the same eleven
  libraries as our own build had.
- **How they are linked.** `sherpa-onnx` and `sherpa-onnx-sys` 1.13.6 are fetched
  from crates.io and patched (`third-party/patches/`,
  [ADR-458](../docs/decisions/ADR-458-fetchedthirdparty.md)): the safe wrapper
  drops its TTS module, and the build script links ten libraries on every
  target, never the three TTS ones, never PortAudio (Koru records audio with
  `cpal`), and only the TTS-free archives
  ([sherpa-onnx.md](../third-party/patches/sherpa-onnx.md)). Each archive is
  pinned by size and SHA-256 in `crates/koru-voice-sherpa/src/artifacts.rs` and
  both `scripts/setup/fetch-voice-runtime.*`, and downloaded like any other
  pinned file; an official build refuses any other bytes.
- **Verified on Windows.** Release builds of `koru-voice-worker.exe`,
  `koru-agent.exe` and `koru-com-notification.exe` against the upstream archive
  contain **no** eSpeak NG (none of the strings above, nor `espeak-ng-data`).
  The only "espeak" bytes left are inside "interface_type**speak**er", an audio
  device name; the runtime they do need is there. The 2026-10-09 nightly had
  about 65 eSpeak strings in each.
- `THIRD-PARTY-NOTICES.txt` no longer lists eSpeak NG or piper-phonemize on any
  platform, and a test holds that no text-to-speech library is attributed.

**Still owed (⚑1):** on a Mac, a release build of the same three binaries
against the macOS archives, the same byte scan, and one dictation with a sherpa
model ([CATCHUP M45](../docs/features/macos/CATCHUP.md)). Builds already
distributed (the nightlies) contained eSpeak NG. They went to our own devices
and to no customers, which is worth noting rather than acting on.

## Vendored in this tree

| Path | License | Status |
| --- | --- | --- |
| `third-party/patches/onenote_parser.patch` | MPL-2.0, © Markus Siemens | Patches `onenote_parser` 2.0.0, fetched from crates.io. Its package carries no license file, so its notice is a record holding the repository's `LICENSE` (2026-10-14); Koru's changes in `onenote_parser.md` ✅ |
| `crates/koru-blocks/assets/TypeScript.sublime-syntax` | **Apache-2.0, © Microsoft** (TypeScript-Sublime-Plugin, machine-converted by `bat`) | Its README said "MIT, Sublime HQ". That was wrong, and bat's history shows the source. Corrected; it is in every package's notices, and the file itself is now fetched from bat v0.24.0 rather than committed ✅ |
| `apps/android/gradlew*`, `gradle/wrapper/gradle-wrapper.jar` | Apache-2.0, Gradle authors | Build tooling, not shipped; fetched (Gradle 9.8.1), not committed ✅ |
| `contracts/gen/` | Ours, generated from our `.proto` | ✅ |

## Fonts (fetched at build time, shipped in every package)

All OFL-1.1 except Phosphor (MIT). Each family's `OFL.txt`, with its copyright
line and Reserved Font Names, is in `licences/bundled/font-*.txt`, taken from
`google/fonts` at the revision `scripts/setup/fonts.manifest` pins. A test fails if
`FONTS` gains a face that no bundled file names ✅.

The Android app bundles three static faces of the same OFL families (Inter
Regular and Medium, Manrope SemiBold), fetched from Google Fonts into
`assets/fonts/android/` and pinned by SHA-256 in `scripts/setup/fonts.manifest`. Their
notices are `font-inter-android.txt` and `font-manrope-android.txt`, marked
`Platforms: android` so no desktop package lists them; a test fails if the
manifest pins an Android face that no such notice names ✅. The APK carries
them in its generated `THIRD-PARTY-NOTICES.txt` ([§ Mobile](#mobile-3)) ✅.

## Spell dictionaries (⚑2, not shipped yet)

`scripts/setup/fetch-dictionaries.*` fetches seven Hunspell dictionaries from
LibreOffice's `dictionaries` repository (`libreoffice-24.8.2.1`) for
development, and Settings can download them. **No package carries them yet**;
shipping them waits on the design below. These are the licenses as each
dictionary's own README or license file states them:

| Language | Upstream | License | Can we ship it? |
| --- | --- | --- | --- |
| en-US | SCOWL (Kevin Atkinson) | BSD-style permissive, with notice | Yes, with the notice |
| en-GB | Marco Pinto | **LGPL-3.0** (and it includes place names credited "© OpenStreetMap contributors", which are ODbL data) | Yes, as a separate replaceable data file: LGPL allows that, and the OSM credit must be kept |
| es | RLA-ES | GPL-3.0+ **or** LGPL-3.0+ **or** MPL-1.1+, our choice | Yes. Take MPL-1.1+ (or LGPL) |
| fr | Dicollecte | MPL-2.0 | Yes (same license as Koru) |
| pt-BR | Raimundo Moura and contributors | LGPL-3.0 **or** MPL-1.1 | Yes. Take MPL-1.1 |
| de | igerman98 / frami | **GPL-2.0 or GPL-3.0 only** | Only as data shipped beside the app, never compiled in. See below |
| ar | AyaSpell | GPL-2.0+ **or** LGPL-2.1+ **or** MPL-1.1+ | Yes. Take MPL-1.1+ |

**What "data beside the app" means, and why it works for German.** A Hunspell
dictionary is a pair of data files (`.aff` + `.dic`) that our spell engine
reads at run time, the same way a word processor opens a document. Distributing
a GPL data file *alongside* a program that reads it is "mere aggregation"
(GPL-3.0 §5, last paragraph). The GPL then covers the dictionary file and not
Koru, provided the files stay separate, unmodified or published if modified,
with their license and source location beside them. The line we must not cross
is **compiling a GPL dictionary into a binary** (`include_bytes!`) or merging
it with our own word lists into one file. That would make the binary a combined
GPL work, which is exactly the eSpeak NG problem above.

So shipping them needs the design below. **Items 1–3 are enforced by code now
(2026-10-14)**: every fetch and every Settings download carries the upstream
license files beside each dictionary, pinned by size and SHA-256, and records
the declared license, the one Koru takes and a GPL-only flag in the manifest
(`crates/koru-spell/src/sources.rs` is the table both fetch scripts mirror); and
a test fails if any source compiles a `.aff`/`.dic` into a binary or commits
one to the tree:

1. Dictionaries are files in the package (or downloaded on demand), one
   directory per language, never embedded.
2. Each directory carries the upstream README/license file verbatim, and
   `THIRD-PARTY-NOTICES.txt` lists each with its chosen license and source URL.
3. For the tri-licensed ones we record our choice (MPL-1.1+ or LGPL) in the manifest.
4. German either ships as above under GPL, or is replaced by a permissively
   licensed dictionary if one is good enough. That is a quality call for the
   spell-check work, not a licensing blocker.

## Mobile (⚑3)

- **Android:** AndroidX, Compose, kotlinx-coroutines, protobuf-kotlin-lite and
  ZXing core are Apache-2.0 (protobuf BSD-3). **Google Play services**
  (`play-services-code-scanner`, `credentials-play-services-auth`, `googleid`)
  are **proprietary Google SDKs**. MPL permits combining them in a larger work,
  but a fully free build (F-Droid) would need them behind a flavor.
  ML Kit (`com.google.mlkit:*`, pulled in by the code scanner) is likewise
  proprietary, under the ML Kit Terms of Service.
- **iOS:** `apple/swift-protobuf` (Apache-2.0).
- ✅ **Android's notices are generated** into the APK by Gradle through
  `koru-release notices android` and shown in Settings → Open-source licenses
  ([README.md § Android](README.md#android)): 177 Maven libraries in a debug
  build, plus the native library's crates and the two bundled faces.
- **iOS's are generated, not yet built** (2026-10-14): `koru-release notices ios`
  writes the same four files as Android (the staticlib's crates and
  swift-protobuf, Apache-2.0); an Xcode build phase puts them in the app
  bundle, and Settings → Open-source licenses shows them. None of it has been
  compiled on a Mac yet ([CATCHUP I18](../docs/features/macos/CATCHUP.md)).

## First-party assets: where they came from

| Asset | Origin | Status |
| --- | --- | --- |
| Logo and app icons (`assets/logo*.png`, `.ico`/`.icns`, mobile icons) | **Made by Steven Liechti with Nano Banana 2** (Google's image model; maintainer, 2026-10-11) | ✅ Ours, through the founding agreement's IP assignment. A trademark protects the logo as Koru's brand. Copyright in a purely AI-generated image is weak in the US, so the trademark is what TRADEMARKS.md relies on. Keep a copy of Google's generative-AI terms, which allow commercial use of outputs, with the logo files |
| Reminder sounds (`assets/reminders/*.wav`, ten files) | **Generated by Koru, not downloaded.** Evidence below | ✅ Ours |
| `assets/emoji/aliases.json` | Hand-written in `cff56464c` (2026-08-02): six emoji with Koru's own keywords (🚀 → launch, startup, deploy, ship) | ✅ Ours. Too small and too specific to be derived from a dataset |
| `crates/koru-voice-bench/fixtures/smoke/*.wav` | Synthetic tone bursts (their `PLACEHOLDER` file says so) | ✅ |

**The reminder sounds** arrived in one commit, `ef61b812a` (2026-09-01,
"Reminders: maintainer signs off PLAN.rev3; final tone assets land"). The plan before
it said *"The maintainers will generate the tone assets"*. Nothing in the history names a
download, a sound library or a license, and the files themselves look generated:

- **They carry no metadata at all.** Each is a bare 44-byte WAV header plus
  samples: no `LIST`/`INFO` chunk, no encoder tag. Audio exported from an editor
  (Audacity, a DAW) or ffmpeg, or downloaded from a sound library, almost
  always carries one. A script writing WAV directly (Python's `wave` module)
  produces exactly this.
- **Every file peaks at exactly 16,422**, which is −6 dBFS to the sample. That is
  one normalization applied to all ten by the same program.
- **Their frequencies are exact musical notes.** `01_Koru_Default` is E5/C5/B5/G5
  (659/524/987/785 Hz), `02_Soft` is A4 and its harmonics (440/660/881/1319),
  `06_Calm` is a C-major triad (523/659/784). These are additive sine tones,
  synthesized, not recordings.
- Mono, 44.1 kHz, 16-bit, 0.34–0.76 s each: the same format throughout.

So they were synthesized, by a script or a tone generator, which is consistent
with "the maintainers will generate" and with an AI assistant writing the synthesis.
Nothing points to a third-party source.

## Ownership

- **3,115 of 3,133 commits** are by Steven Liechti. The other 18 are by the AI
  coding tools "Synara" (16) and "T3 Code" (2), run by the same owner. No
  other human has contributed.
- **The copyright holder is Koru Works, Inc.** (`NOTICE`, TRADEMARKS.md). The
  code was written by Steven Liechti, and the company's founding agreement
  assigns his work to it (maintainers, 2026-10-11) ✅.

## Settled

- ✅ Release notices: generated and required in every package.
- ✅ TypeScript grammar: provenance corrected, license beside it.
- ✅ Copyright holder: Koru Works, Inc.
- ✅ Contact addresses: `*@koru.do` reaches the maintainers.
- ✅ Trademark search: USPTO (TESS) shows no "Koru" in software/SaaS that would
  compete in the workspace/AI space (maintainers, 2026-10-11). That covers the US. EUIPO
  and WIPO searches are worth repeating before EU launch.

## ⚑ Open

1. **eSpeak NG on macOS: fixed in the tree, a Mac link test owed.** Every
   platform links upstream's TTS-free archive. Before any public macOS release,
   build the three binaries on a Mac, scan them and dictate once
   ([CATCHUP M45](../docs/features/macos/CATCHUP.md)).
2. **Dictionaries.** Separate files with their licenses, never compiled in, is
   now enforced; packaging them and deciding German remain.
3. **iOS: done in the tree, a Mac build owed.** Android's notices ship in the
   APK (2026-10-13); iOS's generator, build phase and screen exist but have not
   been built or run ([CATCHUP I18](../docs/features/macos/CATCHUP.md)). (Play
   services stay: no F-Droid flavor for now, maintainer 2026-10-11.)
4. Optional: have counsel confirm the SPDX-only file header. The full MPL
   Exhibit A notice is in `NOTICE` and `LICENSE`. This document is not legal advice.
5. **Upstream license requests.** Five drafted in
   [UPSTREAM-ISSUES.md](UPSTREAM-ISSUES.md), not filed yet. Filing them only
   lets a record give way to the crate's own file; nothing waits on it.
