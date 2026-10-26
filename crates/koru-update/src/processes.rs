// SPDX-License-Identifier: MPL-2.0

//! Which processes are running from under a directory, on Linux.
//!
//! **The Linux quiesce proof is process absence**, as it is on macOS, and for
//! the same reason: Linux renames, overwrites and write-opens a running image
//! without complaint, so the Windows probe — *may I open this for writing* —
//! answers yes for a live service and proves nothing. What has to be true
//! before an update moves a single file is that no process is executing one.
//!
//! `/proc/<pid>/exe` is the kernel's own answer to *which file is this process
//! running*: the resolved path, followed through renames — a binary moved into
//! `.previous/` still reads as that path, and one deleted reads as its old path
//! with ` (deleted)` appended. Both stay under the installation, so neither
//! slips past the check. It names the image even for `koru-agent --worker …`,
//! where the command line would only say which role it is playing.
//!
//! **A process this user may not inspect is skipped**, and that is the one
//! assumption here. Every Koru process an installation starts runs as the
//! person who owns it, and so does the updater asking; a process whose `exe`
//! link answers `EACCES` belongs to another account. An installation that
//! another account executes from is not one this transaction defends.
//!
//! The updater uses this to prove quiet, and the Agent to find the MCP bridges
//! an MCP host started from this installation so it can end them before the
//! handoff. Both read the same table the same way.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Every process other than this one whose executable image is under `dir`,
/// as `(pid, image)`.
///
/// An error means the table could not be read at all, and a caller proving
/// quiet must treat it as *not quiet*: claiming absence on a broken probe is
/// how an installation is replaced under a live process.
pub fn running_from(dir: &Path) -> Result<Vec<(u32, PathBuf)>, String> {
    let dir = fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    running_from_in(Path::new("/proc"), &dir)
}

fn running_from_in(proc_root: &Path, dir: &Path) -> Result<Vec<(u32, PathBuf)>, String> {
    let own = std::process::id();
    let mut found = Vec::new();
    let entries =
        fs::read_dir(proc_root).map_err(|e| format!("reading {}: {e}", proc_root.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("reading {}: {e}", proc_root.display()))?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own {
            continue;
        }
        match fs::read_link(entry.path().join("exe")) {
            Ok(image) if image.starts_with(dir) => found.push((pid, image)),
            Ok(_) => {}
            // Gone between the listing and the read, or a kernel thread,
            // which has no image at all.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            // Another account's process; see the module comment.
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
            Err(error) => {
                return Err(format!("reading the image of process {pid}: {error}"));
            }
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests;
