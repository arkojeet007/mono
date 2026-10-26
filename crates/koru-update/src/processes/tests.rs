// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::os::unix::fs::symlink;
use std::time::{Duration, Instant};

/// The table as the kernel lays it out, with the images pointing wherever a
/// case needs: one under the installation, one renamed into its backup, one
/// deleted from it, one elsewhere, and the non-process entries `/proc` also
/// holds.
#[test]
fn only_images_under_the_directory_are_reported() {
    let table = tempfile::tempdir().unwrap();
    let install = Path::new("/opt/koru");
    for (name, image) in [
        ("101", "/opt/koru/koru-agent"),
        ("102", "/opt/koru/.previous/build-1-x/koru-agent"),
        ("103", "/opt/koru/koru-mcp (deleted)"),
        ("104", "/opt/koru-other/koru-agent"),
        ("105", "/usr/bin/bash"),
    ] {
        fs::create_dir(table.path().join(name)).unwrap();
        symlink(image, table.path().join(name).join("exe")).unwrap();
    }
    // A kernel thread has an `exe` that reads as nothing; `self` and the
    // other named entries are not processes.
    fs::create_dir(table.path().join("106")).unwrap();
    fs::create_dir(table.path().join("self")).unwrap();
    fs::write(table.path().join("uptime"), b"1 1").unwrap();

    let found = running_from_in(table.path(), install).unwrap();
    let pids: Vec<u32> = found.iter().map(|(pid, _)| *pid).collect();
    assert_eq!(pids, [101, 102, 103], "{found:?}");
}

#[test]
fn an_unreadable_table_is_an_error_never_quiet() {
    let missing = tempfile::tempdir().unwrap().path().join("gone");
    assert!(running_from_in(&missing, Path::new("/opt/koru")).is_err());
}

/// Start a binary this process has just written. Another test thread can fork
/// while the copy's write handle is still open in this process, and that child
/// holds it until it execs, so `ETXTBSY` is a passing state, not a failure.
fn spawn_fresh(command: &mut std::process::Command) -> std::process::Child {
    let started = Instant::now();
    loop {
        match command.spawn() {
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && started.elapsed() < Duration::from_secs(5) =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            result => return result.unwrap(),
        }
    }
}

/// Not a test: the process the next one runs, from a copy of this harness.
#[test]
fn sleeping_child() {
    if std::env::var_os("UPDATE_TEST_SLEEPING_CHILD").is_some() {
        std::thread::sleep(Duration::from_secs(30));
    }
}

/// The real table: a process started from a copied binary is found while it
/// runs and gone once it has exited. The binary is this test harness rather
/// than `/bin/sleep`, which on a multicall coreutils refuses to run under any
/// other name and exits before it can be seen.
#[test]
fn a_running_copy_is_found_and_its_exit_is_seen() {
    let install = tempfile::tempdir().unwrap();
    let binary = install.path().join("koru-test-sleeper");
    fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
    let mut child = spawn_fresh(
        std::process::Command::new(&binary)
            .args(["--exact", "processes::tests::sleeping_child"])
            .env("UPDATE_TEST_SLEEPING_CHILD", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    );
    let pid = child.id();
    // Between fork and exec the child still runs this test's image, so the
    // first reads may not see it yet.
    let started = Instant::now();
    let seen = loop {
        let found = running_from(install.path()).unwrap();
        if found.iter().any(|(found, _)| *found == pid) {
            break true;
        }
        if started.elapsed() > Duration::from_secs(10) {
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(seen, "the running copy was never reported");
    assert!(
        !running_from(install.path())
            .unwrap()
            .iter()
            .any(|(found, _)| *found == pid),
        "a reaped process is gone"
    );
    let elsewhere = tempfile::tempdir().unwrap();
    assert!(running_from(elsewhere.path()).unwrap().is_empty());
}
