// SPDX-License-Identifier: MPL-2.0

//! *Manage documents over MCP*: the command an MCP host runs to reach this
//! installation, shown on the row and put on the clipboard by *Copy*.
//!
//! **The bridge beside this executable**, as a chat's agent starts it
//! (`koru-session-host`'s `chat_bridge`): a nightly window hands out the
//! nightly bridge, never one found on `PATH`. `--root` goes with it when this
//! launch was given one, or the host would serve a different store from the
//! one this window shows.

use std::path::Path;

/// The bridge's file name — `koru_update::exe::MCP`, which this app does not
/// link.
const MCP_EXE: &str = if cfg!(windows) {
    "koru-mcp.exe"
} else {
    "koru-mcp"
};

/// The command line for this installation, ready to paste.
pub(crate) fn command() -> String {
    // Absolute, because the host that runs it starts in a directory of its own.
    let root = koru_workspace_host::cli::root_from_args()
        .map(|root| std::path::absolute(&root).unwrap_or(root));
    match std::env::current_exe() {
        Ok(exe) => command_line(&exe.with_file_name(MCP_EXE), root.as_deref()),
        // Unreadable only on a broken platform; the bare name still says what
        // to run.
        Err(_) => command_line(Path::new(MCP_EXE), root.as_deref()),
    }
}

/// `<bridge> serve --stdio [--root <root>]`, each path quoted when it has a
/// space in it.
pub(super) fn command_line(bridge: &Path, root: Option<&Path>) -> String {
    let mut line = format!("{} serve --stdio", quoted(bridge));
    if let Some(root) = root {
        line.push_str(" --root ");
        line.push_str(&quoted(root));
    }
    line
}

fn quoted(path: &Path) -> String {
    let text = path.display().to_string();
    if text.contains(' ') {
        format!("\"{text}\"")
    } else {
        text
    }
}
