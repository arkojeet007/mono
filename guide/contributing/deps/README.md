# Library primers

One page per dependency, explaining the concepts as this codebase uses them —
for someone reviewing the code who has not used the library before.

| | |
| --- | --- |
| [LORO.md](LORO.md) | the CRDT: containers, marks, sync, undo, time travel |
| [PARLEY.md](PARLEY.md) | text layout: shaping, fallback, inline boxes, selection |
| [SYNTECT.md](SYNTECT.md) | syntax highlighting: grammars, themes, the `Highlighter` trait |
| [VELLO.md](VELLO.md) | GPU rendering: scenes, transforms, glyphs, surfaces |
| [WINIT.md](WINIT.md) | windows, the event loop, input, DPI, the macOS app delegate |
| [ACCESSKIT.md](ACCESSKIT.md) | the accessibility bridge: the version pin that tracks winit, the adapter's ordering rules, which UIA property a locator lands on, and why an advertised action set is not a filter |
| [IROH_RELAY.md](IROH_RELAY.md) | the relay server library: ACME for two hostnames, the auth seam, health, metrics, QAD |
| [REQWEST.md](REQWEST.md) | HTTP response lengths, especially why HEAD must read the raw header |
| [WIN32.md](WIN32.md) | global held keys, secure desktops, exact focused controls, Unicode insertion, why clipboard fallback is deferred, and the client half: which features `VARIANT` hides behind, why a wildcard locator is a scan, and the foreground lock that makes an input proof safe to leave in the tree |
| [WINDOWS_CAPTURE.md](WINDOWS_CAPTURE.md) | HWND Graphics Capture and H.264 encoding for the Plan 4.1 sample recorder; the crate default is HEVC |
| [KEYRING.md](KEYRING.md) | OS credential storage, scoped OAuth entry names, and test seams |
| [ANDROID_CREDENTIALS.md](ANDROID_CREDENTIALS.md) | explicit Google chooser, Activity lifetime, provider clearing and correlated native effects |
| [NOTIFICATIONS.md](NOTIFICATIONS.md) | Windows/macOS local notifications, activation, sound, and packaging constraints |
| [WRY.md](WRY.md) | a native WebView as a child of our window: creation blocks, data directories, what IPC carries, what the page-load events cannot say |
| [INTERCOM.md](INTERCOM.md) | the Web Messenger inside a WebView: the domain allowlist fails silently, the 450 px layout switch, the JavaScript object is swapped after boot, hiding is what keeps a badge |
| [CRON.md](CRON.md) | cron's seven-field parser behind Koru's bounded five-field recurrence language |
| [MERMAN.md](MERMAN.md) | Mermaid parsing and layout behind one sealing crate: the features and the licenses they carry, the host text measurer, what the layout JSON leaves out and the patch that adds it, the config keys that pass through |

The language itself: [Rust through Koru examples](../RUST.md).

The workspace [Cargo manifest](../../../Cargo.toml) pins the dependencies.
For changes to that set, follow the [proposal process](../../../GOVERNANCE.md#proposals-and-decisions).

Not covered, because they need no explanation beyond their name: `chrono`
(dates), `uuid` (ids), `pollster` (blocks on one `async` call — see
[VELLO.md](VELLO.md#surfaces-and-devices)), `peniko` (the shared `Color` and
`Blob` vocabulary between parley and vello).

ACP wire types and lifecycle traps: [ACP.md](ACP.md).
