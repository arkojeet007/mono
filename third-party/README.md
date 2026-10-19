# third-party

Koru builds with some third-party code that this repository does not commit,
because it can be fetched. What is committed is what Koru adds:

| | |
| --- | --- |
| [`fetch.manifest`](fetch.manifest) | Every fetched thing, pinned by sha256 |
| [`patches/<name>.patch`](patches/) | Koru's change to a crate, as a diff against the published `.crate` |
| [`patches/onenote_parser.md`](patches/onenote_parser.md), [`patches/sherpa-onnx.md`](patches/sherpa-onnx.md), [`patches/merman-render.md`](patches/merman-render.md) | What each patch changes and why |

The manifest has two kinds of entry:

- **`crate`**: a crates.io release that Koru changes. `onenote_parser` 2.0.0,
  `sherpa-onnx` / `sherpa-onnx-sys` 1.13.6, and `merman-render` 0.9.0. Each is
  placed, patched, in `third-party/crates/<name>/`, and the workspace
  `Cargo.toml` names it there by path (`onenote_parser`) or through
  `[patch.crates-io]` (the sherpa-onnx pair and `merman-render`).
- **`file`**: an upstream file used as it is, placed at its own path in the
  tree, which is gitignored. The TypeScript grammar `koru-blocks` compiles in
  (`crates/koru-blocks/assets/TypeScript.sublime-syntax`, from bat v0.24.0) and
  the Android build's Gradle wrapper (`apps/android/gradlew`,
  `gradlew.bat` and `gradle/wrapper/gradle-wrapper.jar`, Gradle 8.11.1).

`scripts/setup/fetch-third-party.ps1` (Windows) and `scripts/setup/fetch-third-party.sh`
(macOS, Linux) download each entry, refuse anything that does not match its pin,
apply a crate's patch with `git apply`, and put each in place.

**Every cargo command needs them**, so `koru.bat` and `./koru.sh` run the fetch
before every verb but `help`. It costs nothing once done: the script returns at
once while `third-party/crates/.fetched` matches `fetch.manifest`, and fetches
again only an entry whose line changed. **Run it yourself first** if you call
`cargo` directly on a fresh clone (until then cargo stops with `patch location
... third-party/crates/sherpa-onnx does not contain packages matching`), or
`gradlew` in `apps/android/`, or open that project in Android Studio.

`third-party/sherpa-onnx/` is unrelated: it holds the native sherpa-onnx
archive, upstream's TTS-free build, that `scripts/setup/fetch-voice-runtime.*`
downloads and checks against its pin in
`crates/koru-voice-sherpa/src/artifacts.rs`, and is gitignored too.

## Why not commit them, or depend on a fork

A crate's source is upstream's, byte for byte, apart from the patch, so
committing it put other people's code in the history to carry a few hundred
lines of ours, and made our change something a reviewer had to find by diffing
against crates.io. The grammar and the wrapper are upstream's unchanged, and
the wrapper jar is a binary. A git dependency on a fork would avoid the script
for the crates, but every crate comes from crates.io (`deny.toml`,
`[sources]`), and a fork moves the patch out of this repository's review.
[ADR-458](../docs/decisions/ADR-458-fetchedthirdparty.md) records the decision.

## Changing a patch

1. Fetch (any `koru.bat` verb does it), then edit the crate in
   `third-party/crates/<name>/`. Build and test against it as usual; the fetch
   leaves an edited crate alone while its pins are unchanged.
2. Rewrite the patch from your edit:

   ```bat
   scripts\setup\fetch-third-party.ps1 -UpdatePatch <name>
   ```

   or `bash scripts/setup/fetch-third-party.sh --update-patch <name>`. Both diff the
   published `.crate` against your tree and print the new patch's sha256.
3. Put that sha256 in the crate's line in `fetch.manifest`. A patch that does
   not match its pin is refused, so the pin is the reviewed record that the
   patch changed.

## Moving to a new upstream version

**A crate:** change its version and crates.io sha256 in `fetch.manifest` (the
checksum is in `https://crates.io/api/v1/crates/<name>/<version>`), and fetch.
If the patch still applies, rewrite it anyway (`-UpdatePatch`) so its context
lines match the new release, and pin it. If it no longer applies, the fetch
says so, and has already downloaded and verified the new `.crate` into
`third-party/crates/.download/`. Unpack that over a clean
`third-party/crates/<name>/`, apply the old patch with `git apply --reject` to
see which hunks need redoing, make them by hand, delete the `.rej` files, and
rewrite the patch as above. Each crate's notes in `patches/` list anything else
to bump with it.

**A file:** change its URL and sha256. For the Gradle wrapper, change all three
files to the same Gradle tag and `distributionUrl` in
`apps/android/gradle/wrapper/gradle-wrapper.properties` to match; the
jar's sha256 must equal the one Gradle publishes beside the release
(`https://services.gradle.org/distributions/gradle-<version>-wrapper.jar.sha256`).

## Licenses

Each crate keeps its own license, which its `.crate` carries:
`onenote_parser` is MPL-2.0, the sherpa-onnx crates are Apache-2.0 and
`merman-render` is MIT OR Apache-2.0. Koru's patches are under the license of
the code they change. The grammar and the
Gradle wrapper are Apache-2.0. The license texts that ship in every package are
in `crates/koru-release/licences/`;
[`licensing/`](../licensing/README.md) has the full inventory.
