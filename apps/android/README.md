# Koru Android

Document text editing is available behind the reader's pencil button. Tap a
text block to type. Saves wait for a 1.5-second pause; Done flushes pending text,
hides the keyboard and returns to reading after saves finish. Leaving the
document or backgrounding also flushes pending text. Enter creates and focuses
a new paragraph (code keeps a newline); reading and editing share typography
and paragraph spacing. `run-local --device <name>` from this directory updates
a device in place, keeping its app storage and account configuration.

For sign-in failures, see
[Google account build configuration](#google-account-build-configuration).

**Sign in with Apple** has no Android sheet. The account worker publishes a
browser prompt; the app opens it once per request id with a plain
`ACTION_VIEW` (only https pages on `appleid.apple.com` or
`accounts.google.com`), and the result returns as the verified App Link
`https://www.koru.do/v1/oauth/app/callback`. `BrowserSignInReturnActivity`, a
no-display trampoline, hands that URL to the application-owned account service
and brings the existing `MainActivity` forward, so its launch mode and the
`link-device` link are unchanged. A link with no page waiting is ignored; the
URL carries a one-use grant and is never logged. Backing out of the browser
leaves the wait on screen until **Cancel sign-in**.
`run-local` enables account diagnostics automatically for its launch, including
native chooser/vault stages and the shared client's categorized HTTP failures.

**Devices → Linked devices → Relink** immediately shows **Waiting to relink**
and rejects repeat taps. The saved account's enrollment targets and retry state
keep that wait across recreation and beyond ten minutes while work is pending;
terminal failures retain their account diagnostics. The row remains visible
with a disabled action while the account and authorized Profiles load.

This is the native Kotlin and Jetpack Compose application. The device-linking
slice embeds Koru's Rust Store and iroh replicator, protects device seeds with
Android Keystore, and reads replicated profiles, workspaces, and complete
documents from the device-local SQLite Store. Document bodies and synchronized
images are viewable now, including links and every current block kind; editing
follows later.

## QR scanner readiness and diagnostics

Scan QR code uses Google's code-scanner UI in Google Play services. Koru does
not need its own camera permission; the scanner owns camera access. The Lenovo
tablet failure showed `No registered Chimera impl` for Google's barcode UI while
Play services already had camera permission. The previous handler listened only
for success, leaving that failure invisible.

The scanner now explicitly requests its Play services module and waits for
installation completion before opening the camera. A successful install request
is only an acknowledgement, not readiness. The manifest also requests
`barcode_ui` delivery for Play installs. Preparation is bounded to one minute,
has Cancel, and retires when the user leaves or backgrounds the screen. Repeated
taps cannot start overlapping scans. A failed scan shows localized retry/paste
guidance; scanner cancellation is not an error. QR contents and SDK exception
messages never enter logs.

`KoruMobile` logcat includes `QR scanner stage=prepare|scan` and categorical
outcomes, plus a numeric SDK code on failure. If preparation succeeds but Google's
UI still fails, use that scan-stage code to distinguish an installed Play services
problem from module download/readiness. Do not reset Koru storage to repair it.
References: [Google code scanner](https://developers.google.com/ml-kit/vision/barcode-scanning/code-scanner)
and [module installation](https://developers.google.com/android/guides/module-install-apis).

## Share content into Koru

Koru appears in Android's share sheet for single and multiple items of every
MIME type. Share a URL, text, image or document, choose a Koru profile and
workspace, then press **Save**. Each item becomes a `kind == "file"` document in
the embedded Store, so it appears under the Documents screen's **Files** tab and
replicates to the linked desktop like anything else.

A share is staged before it is imported: the payload is streamed to a scratch
file in `cacheDir` while the sender's URI grant is alive, handed to the native
importer as a path, and deleted afterwards. The grant and the Store commit run
on different clocks, and a shared video must not have to fit in the JVM heap on
its way to a native call that reads from disk anyway.

The URL path can be exercised directly with `adb`:

```powershell
$adb = "$env:LOCALAPPDATA\Android\Sdk\platform-tools\adb.exe"
& $adb shell am start `
  -a android.intent.action.SEND `
  -t text/plain `
  --es android.intent.extra.TEXT 'https://koru.app/' `
  -n do.koru.app/app.koru.mobile.ShareActivity
```

For content URIs and multiple files, use Share from Android's Files app so its
temporary read grant follows the intent.

## Requirements

- JDK 17 or newer. The project compiles Java/Kotlin to Java 17 bytecode.
- Android SDK Platform 37 (`platforms;android-37.0`). AndroidX Core 1.19
  needs it to compile against; the app still targets API 36.
- Android NDK `30.0.15729638`.
- Rust Android targets `aarch64-linux-android` and `x86_64-linux-android`.
- An API 29 or newer emulator or physical device.

Install the Rust targets once with:

```powershell
rustup target add aarch64-linux-android x86_64-linux-android
```

The Gradle debug APK task builds `koru-mobile-native` for both ABIs and packages
the resulting `libkoru_mobile.so` files automatically. The first build is slow;
later builds reuse Cargo's target artifacts.

## Run from the command line on Windows

Use `koru.bat` as the main entry point from this directory. It discovers a JDK
17+ and the Android SDK, then sets `JAVA_HOME`, `ANDROID_HOME`, and
`ANDROID_SDK_ROOT` for the command it runs.

```powershell
cd <your clone>\apps\android

.\koru.bat doctor
.\koru.bat avds
.\koru.bat emulator Pixel_8_API_35
.\koru.bat devices
.\koru.bat run
```

`run` builds the debug APK, installs it on the one ready device or emulator,
launches Koru, and then streams the filtered `KoruMobile` log. Press `Ctrl+C`
to stop the log stream; the application keeps running. The most useful commands
are:

Running `koru.bat` without a command prints help and does not build, install, or
launch anything. Use `koru.bat run` explicitly when that is what you want.

| Command | Purpose |
| --- | --- |
| `.\koru.bat build` | Build the debug APK. |
| `.\koru.bat run-local` | Load `.env.local` account settings, build, install, launch and stream logs. |
| `.\koru.bat run-prod` | Load `.env.production` hosted HTTPS settings, build, update in place, launch and stream logs. |
| `.\koru.bat test` | Run unit tests. |
| `.\koru.bat check` | Compile the Kotlin application. |
| `.\koru.bat contracts` | Regenerate Rust, Kotlin, and Swift contract bindings. |
| `.\koru.bat install` | Build and install without launching. |
| `.\koru.bat clean-install --device <exact-serial-or-model>` | Build, erase this app installation, then install the current APK on that same device. |
| `.\koru.bat launch` | Launch the installed application. |
| `.\koru.bat avds` | List configured Android Virtual Devices. |
| `.\koru.bat emulator <name>` | Start an AVD in a visible window. |
| `.\koru.bat devices` | List devices, aliases, and report whether each has booted. |
| `.\koru.bat alias` | Ask for a name for each connected device that has none. |
| `.\koru.bat alias <name> [device]` | Name one device so `--device=<name>` finds it. |
| `.\koru.bat stop [serial]` | Stop an emulator. |
| `.\koru.bat logcat` | Stream only Koru's structured logs. |
| `.\koru.bat logcat process` | Stream all logs from the Koru process. |
| `.\koru.bat logcat crash` | Show Android's crash buffer. |
| `.\koru.bat logcat all` | Stream all device logs. |
| `.\koru.bat gradle <args...>` | Pass a task or options to Gradle. |

Per-object receive logs are opt-in because a large initial sync can produce a
lot of output. Enable them for both the launched Android app and the desktop
Store from the same terminal:

```powershell
$env:KORU_PEER_OBJECT_LOGS = '1'
.\koru.bat run
```

Unset `KORU_PEER_OBJECT_LOGS` (or set it to `0`) to disable them. The records
identify profile, workspace and document IDs plus lifecycle state; they never
include document content or replicated bytes.

Run `.\koru.bat help` for the complete command list. Gradle options after
`build`, `test`, `check`, `install`, or `run` are forwarded to the wrapper:

```powershell
.\koru.bat test --stacktrace
.\koru.bat gradle :app:dependencies
```

Add `--contracts` to generate all contract bindings before a build-like
command. The flag is consumed by `koru.bat` and is not passed to Gradle:

```powershell
.\koru.bat build --contracts
.\koru.bat run --contracts
```

Contract generation requires the `buf` CLI on `PATH`. A normal build does not,
so Android-only iteration remains fast when the schemas have not changed.

### Run against the hosted API

From this directory, copy `.env.example` to `.env.production` and set
`koruAccountOrigin` and `koruAccountIssuer` to `https://api.koru.do`.
Set `koruGoogleServerClientId` to the API's existing public **web** OAuth client
ID and keep `koruAccountChannel` the same as the device's current build.
This is an ignored Android configuration file with public values only; use
the Android template, not the API server's secret-bearing environment file.

```powershell
.\koru.bat run-prod --device phone
.\koru.bat run-prod --device tablet
```

Run the second command in another terminal or press Ctrl+C to stop the first
log stream. `run-prod` builds the same debug APK as `run-local`, installs with
`adb install -r` to preserve data, launches, and enables account diagnostics.
It selects `.env.production` without changing `.env.local` or the caller's
environment. Origin and issuer must be hosted HTTPS, including explicit Gradle
overrides. It clears any inherited API forwarding request; ADB is used only
to install, launch and view logs. Disconnect it for
[hosted acceptance S](../../docs/features/oauth/TESTPLAN.md#s-hosted-oauth-release-without-local-api-infrastructure-about-25-minutes).
The APK's actual signing certificate still needs its Android OAuth registration.

The configuration-loader regression checks use a fake runner and need no SDK or
device: `powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\run-configured.tests.ps1`.

### Clean installation for acceptance

`clean-install` requires exactly one explicit `--device` naming a connected
device's complete serial or complete model. It accepts the model token printed
by `devices` (for example `Pixel_9_Pro`) or its spaced form (`"Pixel 9 Pro"`).
Use the exact serial when two devices have the same model. Aliases, substrings,
`KORU_DEVICE`, saved defaults and the interactive device picker are not used.

```powershell
.\koru.bat devices
.\koru.bat clean-install --device emulator-5554
# Or quote a complete model / wireless-debugging serial containing spaces:
.\koru.bat clean-install --device "Pixel 9 Pro"
```

The command prints the selected serial and model before building. A successful
APK build and a second check of the same connected device are required before
uninstalling `do.koru.app`, without keeping its data. Removal discards that
installation's local Store content, trust database and legacy trust files,
Keystore-wrapped device seeds and account refresh vault. It then installs the
new APK on the same explicit `adb -s` serial and reports each completed phase.
It does not launch the app. Optional `--contracts` regenerates bindings as part
of the build; other extra arguments are rejected.

An already absent app skips uninstall, reports that nothing was removed and
installs the current APK. A failed package query is a refusal, not evidence of
absence. Build or target-validation failures leave the installed app in place. An uninstall
failure stops before installation; an install failure after successful removal
explicitly reports that local data has already been erased. Check the selected
device before retrying, and retain serial/model only in the local operator log.

Clean-install rebuilds the APK. Values passed with `-P` on an earlier `build`
command do not persist. For account acceptance, set the reviewed public Gradle
project properties in the same PowerShell session before either command:

```powershell
$env:ORG_GRADLE_PROJECT_koruAccountOrigin = 'https://accounts.example.test'
$env:ORG_GRADLE_PROJECT_koruAccountIssuer = 'https://accounts.example.test'
$env:ORG_GRADLE_PROJECT_koruGoogleServerClientId = 'REVIEWED_WEB_CLIENT.apps.googleusercontent.com'
$env:ORG_GRADLE_PROJECT_koruAccountChannel = 'internal'
.\koru.bat build
.\koru.bat clean-install --device emulator-5554
```

Replace those examples with the reviewed staging coordinates. Both commands
inherit these [standard Gradle project properties](https://docs.gradle.org/current/userguide/build_environment.html#sec:project_properties);
the cleanup command does not recover values from an old APK or previous
command line. No user/global Gradle file needs editing. A new terminal needs
the same environment setup. These are public configuration values, not client
secrets or refresh credentials.

Use clean installation for independent fresh-phone cases in
[`TESTPLAN.md` section 12](../../docs/features/oauth/TESTPLAN.md#12-accept-google-v1-from-website-to-desktop-to-mobile-45-minutes-per-mobile-platform).
Use `install` to upgrade the legacy QR fixture in place. Never clean-install
between an enrollment and its restart, logout/offline-content, process-death,
or upgrade checks: those cases verify that existing state survives.

The launcher orchestration has injectable adb/build/APK ports. Its tests need
only PowerShell and use no real SDK, Gradle process, device or filesystem writes:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\clean-install.tests.ps1
```

### Save machine-local tool paths

Automatic discovery checks the usual Android Studio, JDK, and Android SDK
locations. To save the detected paths explicitly, run:

```powershell
.\koru.bat setup
```

Or provide both paths when they are installed somewhere unusual:

```powershell
.\koru.bat setup 'C:\path\to\jdk' 'D:\Android\Sdk'
```

This writes `koru.local.conf` beside the script. It is ignored by git and is
read only as `key=value` data. You can also set `KORU_JAVA_HOME` and
`KORU_ANDROID_HOME` in the calling environment without creating the file.

When several devices are connected, the runner asks which one a command is for
rather than refusing to run — and it asks for a *name* first, so it only ever
asks once per device:

```
> .\koru.bat run
koru: more than one device is connected.
Connected devices:

  1. Pixel_8 has no name.
     adb-43160DLJH000SV-AN6IkW._adb-tls-connect._tcp
   name it (blank to skip): phone
   [koru] device.phone=... saved in ...\koru.local.conf

  2. Lenovo_TB_J606F has no name.
     adb-HA1HAMMT-zBoAgN (2)._adb-tls-connect._tcp
   name it (blank to skip): tablet
   [koru] device.tablet=... saved in ...\koru.local.conf

   device for this command (number, blank to cancel): 2
[koru] using tablet. Skip this next time with --device=tablet
```

After that, `--device=tablet` picks it outright. `.\koru.bat alias` on its own
runs the same naming walk whenever you like, and a `--device` name the runner
does not know is treated as a name you have not given yet: it lists the devices,
asks which one that is, and saves the answer.

```powershell
.\koru.bat alias                    # name each unnamed device, once
.\koru.bat alias tablet             # name one, picked from a numbered list
.\koru.bat alias tablet Lenovo      # or name one outright, no question asked
.\koru.bat run --device=tablet
```

An alias is letters, digits, `_` and `-`. `alias <name> <device>` matches a
serial or any part of a connected device's model or product name, then writes
`device.<name>=<serial>` into `koru.local.conf`; if it matches nothing or is
left out, you get the numbered list instead of an error. `--device` takes an
alias, a serial, or a name fragment, so `--device=pixel` works without an alias;
a fragment matching two devices is an error rather than a guess. It can go
anywhere after the command, and every device command accepts it — `run`,
`install`, `launch`, `logcat`, `stop`.

Set `KORU_NO_PROMPT=1` for a script or a CI job: an ambiguous device is then an
error again, and nothing waits for an answer.

Serials are matched whole, not by whitespace: a wireless-debugging device can
be called `adb-HA1HAMMT-zBoAgN (2)._adb-tls-connect._tcp`, space and all, and
`devices`, `alias` and `--device` all handle it.

For a default instead of a per-command choice, set `$env:KORU_DEVICE =
'emulator-5554'` for the session, or add `device=emulator-5554` to
`koru.local.conf`. `--device` overrides both.

Nothing sends to two devices at once: build once with `.\koru.bat build`, then
`install` and `launch` each device in turn.

The sections below show the underlying Android SDK and Gradle commands for
troubleshooting or for platforms where `koru.bat` is unavailable.

Android Studio normally discovers the SDK and writes the ignored
`local.properties` file automatically. For command-line use on Windows, set the
JDK and SDK locations for the current PowerShell session:

```powershell
$env:JAVA_HOME = 'C:\dev\jdk\azul-18.0.2'
$env:ANDROID_HOME = "$env:LOCALAPPDATA\Android\Sdk"

$adb = "$env:ANDROID_HOME\platform-tools\adb.exe"
$emulator = "$env:ANDROID_HOME\emulator\emulator.exe"
```

Adjust `JAVA_HOME` if your JDK is elsewhere. All commands below assume the
working directory is this Android repository:

```powershell
cd <your clone>\apps\android
```

On macOS or Linux, use `./gradlew` instead of `./gradlew.bat`; `adb` and
`emulator` are normally under `$ANDROID_HOME/platform-tools/` and
`$ANDROID_HOME/emulator/`.

## Run from Android Studio

1. Open this directory as the Gradle project.
2. Let Gradle sync and install any requested SDK components.
3. Open **Tools > Device Manager** and start an API 29+ virtual device.
4. Select the `app` run configuration and click **Run**.

## List and start emulators from PowerShell

List configured Android Virtual Devices (AVDs):

```powershell
& $emulator -list-avds
```

The SDK command-line manager provides more detail when it is installed:

```powershell
& "$env:ANDROID_HOME\cmdline-tools\latest\bin\avdmanager.bat" list avd
```

Start one of the exact names returned by `-list-avds`:

```powershell
$avdName = 'Pixel_8_API_35'
Start-Process -FilePath $emulator -ArgumentList @('-avd', $avdName)
```

Wait for it to connect and check that Android has finished booting:

```powershell
& $adb wait-for-device
& $adb devices -l
& $adb shell getprop sys.boot_completed
```

The last command returns `1` when boot is complete. If several devices are
connected, pass `-s <serial>` immediately after `adb`; the serial is the first
column from `adb devices -l`:

```powershell
& $adb -s emulator-5554 shell getprop sys.boot_completed
```

Stop an emulator cleanly with:

```powershell
& $adb -s emulator-5554 emu kill
```

## Test, build, install and launch

The Gradle wrapper (`gradlew`, `gradlew.bat`, `gradle/wrapper/gradle-wrapper.jar`)
is fetched, not committed. On a fresh clone, fetch it once from the repository
root; any `koru.bat` verb does the same:

```powershell
scripts\setup\fetch-third-party.ps1
```

Run the Android unit tests:

```powershell
.\gradlew.bat testDebugUnitTest
```

Build a debug APK:

```powershell
.\gradlew.bat assembleDebug
```

The APK bundles three fonts (Inter Regular and Medium, Manrope SemiBold) that
are fetched rather than committed. Run `koru.bat fonts` (or `./koru.sh fonts`)
once at the repository root; until then the build stops at
`checkKoruMobileFonts` and names the missing files.

Every APK also carries its open-source notices in `assets/licenses/`, which
Settings → Open-source licenses shows. `koruNotices<Variant>` writes them with
`cargo run -p koru-release -- notices android` (so the first build compiles
`koru-release` for the host) and needs git for the source URL;
`checkKoruNotices<Variant>` fails `assemble<Variant>` when the APK lacks them.
A release build also refuses a crate without a recorded license, which a debug
build only labels. See [licensing/](../../licensing/README.md#android).

The APK is written to:

```text
app\build\outputs\apk\debug\app-debug.apk
```

Install the current debug build on the running emulator/device:

```powershell
.\gradlew.bat installDebug
```

Launch the installed app explicitly:

```powershell
& $adb shell am start -n do.koru.app/app.koru.mobile.MainActivity
```

To reinstall on or launch against one specific device, use its serial:

```powershell
& $adb -s emulator-5554 install -r .\app\build\outputs\apk\debug\app-debug.apk
& $adb -s emulator-5554 shell am start -n do.koru.app/app.koru.mobile.MainActivity
```

## Inspect logs with adb logcat

Koru's structured development logger uses the `KoruMobile` tag and deliberately
does not log document contents. Show only Koru messages:

```powershell
& $adb logcat -v threadtime KoruMobile:D '*:S'
```

Pairing-code generation now emits either `pairing_offer_created` with the
payload size or `pairing_offer_failed` with the JNI stage, exception type, safe
message, and cause. Replication emits `sync_progress` for catalog, workspace,
and complete milestones. `local_inventory` reports the device-local
profile/workspace/document counts whether the phone is linked or unlinked.
These messages contain IDs and counts, never invitation secrets or document
content.

## Run with local account configuration

Copy [.env.example](.env.example) to `.env.local` in this directory, then fill
its four public build settings. In particular, `koruGoogleServerClientId` is the
API's `KORU_PUBLIC_API_GOOGLE_CLIENT_ID`, the **web** OAuth client ID. The file
does not need a client secret. Git ignores `.env.local`.

```powershell
Copy-Item .env.example .env.local
# Edit .env.local, then:
.\koru.bat run-local
# Or select an existing device alias:
.\koru.bat run-local --device=phone
```

`run-local` reads the file as literal `name=value` entries with optional matching
quotes, blank lines and whole-line `#` comments. It does not expand shell syntax
or inline comments. Missing, duplicate, unknown, blank and placeholder settings
stop the command before device selection or building. Values replace the four
`ORG_GRADLE_PROJECT_*` settings only in the child process; explicit Gradle `-P`
arguments still take precedence. Ordinary `run` keeps its existing behavior.

The example points at an account API on this machine at
`http://localhost:8080`; Koru Cloud's server is not in this repository, so it
is one you run yourself. For a loopback API origin, `run-local` automatically
runs `adb reverse tcp:<port> tcp:<port>`
on the same device selected for installation, including `--device` aliases.
It uses the configured port (including an explicit Gradle origin override) and
stops before installation if forwarding fails. Keep that API running;
rerunning `run-local` restores forwarding after a device reconnect. Remote API
origins skip forwarding. For a remote API use
HTTPS, and keep the issuer exactly equal to the server's configured issuer.
The Android OAuth client/package/signing setup below is still required.

## Google account build configuration

The Android adapters use Credential Manager and a separate Keystore-wrapped
refresh credential. Their fake tests do not need Google configuration. The
account switcher, one-Profile screen and explicit new-account confirmation are
implemented. Add Account resumes a retained sign-in choice. Switching preserves
other held credentials and pending operations; local inventory carries canonical
account binding so offline attribution does not require the hosted API. The
private adapter retains an A SDK/vault request while B is visible and routes the
reply by its original correlation. Real provider, rotation and Keystore
acceptance remain unrun.

An application process may retain its last authorized Profile list while its
Activity is stopped. Each background-to-foreground transition invalidates that
list and the serialized account poll refreshes it with the saved session. This
does not start Credential Manager or require another Google sign-in.

The authorized build supplies four required public Gradle properties, and one
optional fifth:

| Property | Meaning |
| --- | --- |
| `koruAccountOrigin` | HTTPS origin of the reviewed account API |
| `koruAccountIssuer` | Exact issuer expected by that account API's attestation/session configuration |
| `koruGoogleServerClientId` | Existing hosted **web** OAuth client ID, used as token audience |
| `koruAccountChannel` | Credential namespace channel, lowercase ASCII letters/digits/hyphens, at most 40 characters |
| `koruAccountCredentialRollout` | Optional. The public regional credential policy, verbatim from the API's `KORU_PUBLIC_API_CREDENTIAL_ROLLOUT`; it names the region and the issuer a Cloud sync credential is verified against and carries no key. **Absent, the build has no Cloud sync attached**: the account, peer sync and local documents work as before, and nothing arrives from Koru Cloud. |
| `koruBlobByteOrigins` | Optional, and only for a development stack. A local object worker this build may send blob bytes to, beside the compiled-in per-region allowlist — **the phone's half of what `KORU_CLOUD_BLOB_BYTE_ORIGINS` does for the desktop**, because nothing sets an environment variable in an Android process. Set it to the exact origin that stack's object service advertises, scheme and port included, and use this machine's **LAN** address: a phone given `127.0.0.1` dials itself. Only a loopback, private-range or link-local address is admitted; a public host is refused on the device with the production set left standing, and `run-prod` refuses the setting outright. Without it a phone is handed its own stack's byte URL and refuses it, reporting `byte_origin_refused`. |

`run-local` supplies these properties from `.env.local`; other build commands
accept the existing Gradle properties or `ORG_GRADLE_PROJECT_*` environment.
Absent configuration reports `account_configuration_missing`; the app does not
guess a production endpoint. These are build inputs, never Activity extras or
link arguments. Pass reviewed values to the ordinary build, for example:

```powershell
.\koru.bat build '-PkoruAccountOrigin=https://accounts.example.test' '-PkoruAccountIssuer=https://accounts.example.test' '-PkoruGoogleServerClientId=REVIEWED_WEB_CLIENT.apps.googleusercontent.com' '-PkoruAccountChannel=internal'
```

That `-P` example configures only that invocation. For a subsequent
`clean-install`, use the same-shell environment block in
[Clean installation for acceptance](#clean-installation-for-acceptance), so the
rebuild receives the same reviewed values.

The example is a placeholder, not a deployable environment. Configure an
Android OAuth client only after separate authorization, with package
`do.koru.app` and the certificate for the APK being tested. The Android client
ID is not substituted for `koruGoogleServerClientId`. The reviewed hosted web
client and Android client must belong to the intended Google project.

The local debug certificate was inspected with
`.\koru.bat gradle :app:signingReport` on 2026-09-07:

- SHA-1: `3A:66:BD:05:90:33:EE:E7:08:FA:EC:BE:B2:E6:75:FA:F3:61:02:AF`
- SHA-256: `97:8C:28:9D:2E:C8:63:EC:D3:14:BD:A4:F9:FC:BF:2E:82:45:87:01:17:FF:DD:56:36:85:4E:0F:D4:08:97:1B`

This identifies the test build's signing configuration, not a completed Google
journey. Release signing was unconfigured in that report. Re-run the command
for another machine/key or a release APK before requesting console setup.

If the native chooser opens but selecting an account fails, inspect the
correlated Google Play services log. On the 2026-09-07 Lenovo acceptance retry,
`Auth` / `Auth.Api.Credentials` reported `UNREGISTERED_ON_API_CONSOLE`: the
Android package and SHA-1 were not registered correctly. Credential Manager
surfaced this as cancellation, so Koru's `cancelled` code alone did not identify
the configuration problem. In the same Google project as the hosted web client,
open **Google Auth Platform → Clients**, create or correct an **Android** client
using the package and SHA-1 above, then retry sign-in. Keep the existing web
client ID in `.env.local`; the Android client is a separate console registration.

On a separately configured test device, acceptance must observe one explicit
Google chooser through rotation/background-return, another explicit chooser
for collision reauthentication, and full account choice after logout. Inspect
only the Keystore alias namespace/presence and encrypted-file presence. The
namespace for both the Keystore alias and the ciphertext filename is
`do.koru.oauth.refresh-`. v1-form aliases are
`do.koru.oauth.refresh-<v1 entry>`. New account-scoped key aliases use `do.koru.oauth.refresh-v2-<SHA-256 of the exact UTF-8 entry>`.
Android 11's Keystore also stores escaped aliases as filenames; full v2 aliases
failed with `File name too long` on the Lenovo tablet. Existing v2 ciphertext
can still use its original full alias, including refresh, without key replacement.
The exact Rust `v2:<channel>:<origin-digest>:<root-digest>:<device-ID>:<account-ID>`
entry remains AES-GCM authenticated data. The v2 ciphertext filename is:
`do.koru.oauth.refresh-v2-<SHA-256 of the exact UTF-8 entry>.bin` in
`noBackupFilesDir`. Its temporary filename also fits the filesystem component
bound. The authenticated data is never replaced by that hash. See
[ADR-325](../../docs/decisions/ADR-301-325.md#adr-325-androidvaultkeys--new-account-wrapping-keys-use-bounded-physical-aliases).
Existing v1 ciphertext keeps its literal
`do.koru.oauth.refresh-<v1 entry>.bin` filename and remains directly
readable and clearable until the shared account directory completes retirement;
there is no fallback that silently hashes a legacy filename. Entries must match
the canonical v1/v2 shape before storage access. Surviving ciphertext with a
missing or inaccessible key is unavailable, and never causes key replacement.
Never dump credentials, SDK bundles or decrypted bytes. Device seeds retain
their independent `koru-mobile-device-seeds-v1` alias.

Process-death checks require three controlled points: before hosted exchange
commit, after commit but before the vault write, and after durable vault storage.
The first two permit signing in again without a saved continuation; the third
restores the credential. Local Workspace content and existing QR trust must
remain available in each case. Fault-injected JVM/shared Rust tests cover the
policy; the actual device/Keystore observations are still owed.

## Android App Link setup

The manifest accepts `https://koru.do/link-device#...` and requests Android App
Link verification. Android 12 and newer will open the link directly in Koru
only after `koru.do` serves this file over HTTPS without redirects:

```text
https://koru.do/.well-known/assetlinks.json
```

For the current development signing key, its contents are:

```json
[
  {
    "relation": ["delegate_permission/common.handle_all_urls"],
    "target": {
      "namespace": "android_app",
      "package_name": "do.koru.app",
      "sha256_cert_fingerprints": [
        "97:8C:28:9D:2E:C8:63:EC:D3:14:BD:A4:F9:FC:BF:2E:82:45:87:01:17:FF:DD:56:36:85:4E:0F:D4:08:97:1B"
      ]
    }
  }
]
```

Add the release signing fingerprint before distributing a release build. After
publishing the file, reinstall the APK so Android processes the manifest, then
force and inspect verification on a connected test device:

```powershell
.\koru.bat install
$adb = "$env:LOCALAPPDATA\Android\Sdk\platform-tools\adb.exe"
& $adb shell pm verify-app-links --re-verify do.koru.app
& $adb shell pm get-app-links do.koru.app
```

Until the association file is published and verified, the HTTPS URL correctly
remains shareable/clickable but Android may route it to the browser instead of
Koru. Reinstalling alone cannot verify a domain that does not publish the
association file.

To see every message from the currently running Koru process, resolve its PID
and filter logcat by that process:

```powershell
$koruPid = (& $adb shell pidof -s do.koru.app).Trim()
& $adb logcat --pid=$koruPid -v threadtime
```

If `$koruPid` is empty, launch the app first. Useful alternatives:

```powershell
# Show Android's crash buffer.
& $adb logcat -b crash -v threadtime

# Clear old buffered logs, reproduce the issue, then stream fresh logs.
& $adb logcat -c
& $adb logcat -v threadtime KoruMobile:D '*:S'

# Save the current buffered log to a file without continuing to stream.
& $adb logcat -d -v threadtime > .\koru-logcat.txt
```

Press `Ctrl+C` to stop a streaming `logcat` command. With multiple connected
devices, add `-s <serial>` after `$adb` for every command.

## Generated contract sources

The app compiles generated Koru Contract sources from
`../../contracts/gen/kotlin`. Do not hand-edit those files. Regenerate all
contract bindings from this Android directory with:

```powershell
.\koru.bat contracts
```
