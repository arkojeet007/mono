// SPDX-License-Identifier: MPL-2.0

use std::ffi::OsString;
use std::path::Path;

use koru_session_protocol::GridSize;
use koru_session_runtime::{
    EnvironmentPolicy, MAX_QUEUED_WRITE_BYTES, PtyReader, PtyTransport, ReadBatch, SpawnRequest,
    TransportStage,
};

fn size(columns: u16, rows: u16) -> GridSize {
    GridSize::new(columns, rows).expect("test grid is valid")
}

#[cfg(windows)]
fn command(script: &str, cwd: &Path) -> SpawnRequest {
    SpawnRequest::new(
        [
            OsString::from("powershell.exe"),
            OsString::from("-NoLogo"),
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-Command"),
            OsString::from(script),
        ],
        cwd.to_path_buf(),
        size(80, 24),
    )
}

#[cfg(unix)]
fn command(script: &str, cwd: &Path) -> SpawnRequest {
    SpawnRequest::new(
        [
            OsString::from("/bin/bash"),
            OsString::from("-c"),
            OsString::from(script),
        ],
        cwd.to_path_buf(),
        size(80, 24),
    )
}

fn drain(reader: PtyReader) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        reader
            .run(|batch| bytes.extend_from_slice(batch))
            .expect("PTY output reaches EOF");
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

#[test]
fn captures_output_and_exit_exactly_once() {
    let root = tempfile::tempdir().expect("root");
    #[cfg(windows)]
    let request = SpawnRequest::new(
        ["cmd.exe", "/d", "/s", "/c", "echo KORU:READY & exit /b 7"],
        root.path().to_path_buf(),
        size(80, 24),
    );
    #[cfg(unix)]
    let request = command("printf 'KORU:READY\\n'; exit 7", root.path());

    let (mut transport, reader) = PtyTransport::spawn(request).expect("spawn");
    let output = drain(reader);
    let exit = transport.wait().expect("wait");
    let output = output.join().expect("reader thread");
    assert!(output.contains("KORU:READY"), "{output:?}");
    assert_eq!(exit.code, 7);
    assert!(!exit.success);
    assert_eq!(transport.try_wait().expect("repeat wait"), Some(exit));
}

#[test]
fn argv_cwd_and_environment_are_delivered_without_a_shell_join() {
    let root = tempfile::tempdir().expect("root");
    let marker = "KORU value with spaces & metacharacters";
    #[cfg(windows)]
    let script =
        "Write-Output ('ARG:' + $env:KORU_ARG); Write-Output ('CWD:' + (Get-Location).Path)";
    #[cfg(unix)]
    let script = "printf 'ARG:%s\\nCWD:%s\\n' \"$KORU_ARG\" \"$PWD\"";
    let mut request = command(script, root.path());
    request.environment = EnvironmentPolicy::InheritWith {
        set: [(OsString::from("KORU_ARG"), OsString::from(marker))]
            .into_iter()
            .collect(),
        remove: Default::default(),
    };

    let (mut transport, reader) = PtyTransport::spawn(request).expect("spawn");
    let output = drain(reader);
    let exit = transport.wait().expect("wait");
    let output = output.join().expect("reader thread");
    assert!(exit.success, "{exit:?}: {output:?}");
    assert!(output.contains(&format!("ARG:{marker}")), "{output:?}");
    // The same directory, not the same spelling: a CI runner's TEMP is an 8.3
    // short path (C:\Users\RUNNER~1\...) and PowerShell reports the long one.
    let reported = output
        .lines()
        .find_map(|line| line.split_once("CWD:").map(|(_, path)| path.trim()))
        .unwrap_or_else(|| panic!("no CWD line: {output:?}"));
    assert_eq!(
        std::fs::canonicalize(reported).ok(),
        Some(std::fs::canonicalize(root.path()).expect("the root exists")),
        "{output:?}"
    );
}

#[test]
fn cloned_client_writer_does_not_own_the_child() {
    let root = tempfile::tempdir().expect("root");
    #[cfg(windows)]
    let request = command(
        "$line=[Console]::In.ReadLine(); Write-Output ('INPUT:' + $line)",
        root.path(),
    );
    #[cfg(unix)]
    let request = command(
        "IFS= read -r line; printf 'INPUT:%s\\n' \"$line\"",
        root.path(),
    );
    let (mut transport, reader) = PtyTransport::spawn(request).expect("spawn");
    let output = drain(reader);
    let discarded_client = transport.writer();
    drop(discarded_client);
    assert_eq!(transport.try_wait().expect("poll"), None);
    transport
        .writer()
        .write_bytes(b"still-alive\r\n")
        .expect("write");

    let exit = transport.wait().expect("wait");
    let output = output.join().expect("reader thread");
    assert!(exit.success, "{exit:?}: {output:?}");
    assert!(output.contains("INPUT:still-alive"), "{output:?}");
}

#[cfg(windows)]
#[test]
fn interactive_powershell_accepts_a_command() {
    let root = tempfile::tempdir().expect("root");
    let request = SpawnRequest::new(
        [
            OsString::from("powershell.exe"),
            OsString::from("-NoLogo"),
            OsString::from("-NoProfile"),
            OsString::from("-NoExit"),
        ],
        root.path().to_path_buf(),
        size(80, 24),
    );
    let (mut transport, reader) = PtyTransport::spawn(request).expect("spawn");
    let output = drain(reader);
    transport
        .writer()
        .write_bytes(b"Write-Output KORU:POWERSHELL\r")
        .expect("write marker");
    transport
        .writer()
        .write_bytes(b"exit\r")
        .expect("write exit");

    let exit = transport.wait().expect("wait");
    let output = output.join().expect("reader thread");
    assert!(exit.success, "{exit:?}: {output:?}");
    assert!(output.contains("KORU:POWERSHELL"), "{output:?}");
}

#[test]
fn resize_reaches_the_child_before_the_marker_command() {
    let root = tempfile::tempdir().expect("root");
    #[cfg(windows)]
    let request = command(
        "$null=[Console]::In.ReadLine(); Write-Output ('SIZE:' + [Console]::WindowWidth + 'x' + [Console]::WindowHeight)",
        root.path(),
    );
    #[cfg(unix)]
    let request = command("IFS= read -r _; printf 'SIZE:'; stty size", root.path());
    let (mut transport, reader) = PtyTransport::spawn(request).expect("spawn");
    let output = drain(reader);
    transport.resize(size(113, 37)).expect("resize");
    transport
        .writer()
        .write_bytes(b"measure\r\n")
        .expect("write");

    let exit = transport.wait().expect("wait");
    let output = output.join().expect("reader thread");
    assert!(exit.success, "{exit:?}: {output:?}");
    #[cfg(windows)]
    assert!(output.contains("SIZE:113x37"), "{output:?}");
    #[cfg(unix)]
    assert!(output.contains("SIZE:37 113"), "{output:?}");
}

#[test]
fn reader_reports_eof_once() {
    let root = tempfile::tempdir().expect("root");
    let (mut transport, mut reader) =
        PtyTransport::spawn(command("exit 0", root.path())).expect("spawn");
    transport.wait().expect("wait");
    let mut buffer = [0; 1024];
    loop {
        if reader.read_batch(&mut buffer).expect("read") == ReadBatch::Eof {
            break;
        }
    }
    let error = reader
        .read_batch(&mut buffer)
        .expect_err("second EOF is rejected");
    assert_eq!(error.stage, TransportStage::InvalidRequest);
}

#[test]
fn dropping_the_owner_terminates_the_process_tree() {
    let root = tempfile::tempdir().expect("root");
    let marker = root.path().join("descendant.pid");
    #[cfg(windows)]
    let script = format!(
        "$p=Start-Process powershell.exe -ArgumentList '-NoLogo','-NoProfile','-Command','Wait-Event' -PassThru; Set-Content -LiteralPath '{}' -Value $p.Id; Wait-Event",
        marker.display()
    );
    #[cfg(unix)]
    let script = format!("sleep 300 & printf '%s' $! > '{}'; wait", marker.display());
    let (transport, _reader) = PtyTransport::spawn(command(&script, root.path())).expect("spawn");

    let descendant = wait_for_pid_marker(&marker);
    drop(transport);
    assert_process_stops(descendant);
}

#[test]
fn interrupt_reaches_the_foreground_pty_process() {
    let root = tempfile::tempdir().expect("root");
    // Koru's sessions worker descends from a process started with
    // `CREATE_NEW_PROCESS_GROUP`, so it ignores Ctrl+C, and every process it
    // creates inherits that unless the transport clears it. Put the test in
    // the same position rather than depend on how the runner was launched
    // (from an agent's shell it already is; from a person's terminal it is not).
    #[cfg(windows)]
    assert_ne!(
        unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 1) },
        0,
        "ignore Ctrl+C the way the sessions worker does"
    );
    #[cfg(windows)]
    let request = command(
        "Write-Output 'KORU:INTERRUPT-READY'; while ($true) { Start-Sleep -Seconds 30 }",
        root.path(),
    );
    #[cfg(unix)]
    let request = command(
        "trap 'printf KORU:INTERRUPTED\\n; exit 130' INT; printf 'KORU:INTERRUPT-READY\\n'; while :; do read _; done",
        root.path(),
    );
    let (mut transport, reader) = PtyTransport::spawn(request).expect("spawn");
    let (marker_tx, marker_rx) = std::sync::mpsc::sync_channel(1);
    let output = std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut marker_sent = false;
        reader
            .run(|batch| {
                output.extend_from_slice(batch);
                if !marker_sent && String::from_utf8_lossy(&output).contains("KORU:INTERRUPT-READY")
                {
                    marker_sent = true;
                    marker_tx.send(()).expect("marker receiver");
                }
            })
            .expect("reader reaches EOF");
        String::from_utf8_lossy(&output).into_owned()
    });
    marker_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("foreground marker");
    transport.writer().interrupt().expect("interrupt enqueue");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if transport.try_wait().expect("exit poll").is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            transport
                .terminate()
                .expect("cleanup after failed interrupt");
            panic!("foreground process ignored Ctrl+C");
        }
        std::thread::yield_now();
    }
    let exit = transport.wait().expect("collect exit");
    let output = output.join().expect("reader thread");
    assert!(
        !exit.success,
        "interrupt unexpectedly succeeded: {output:?}"
    );
}

#[test]
fn repeated_terminate_reclaims_every_process_and_endpoint() {
    let root = tempfile::tempdir().expect("root");
    for _ in 0..10 {
        #[cfg(windows)]
        let request = SpawnRequest::new(
            ["cmd.exe", "/d", "/q", "/k"],
            root.path().to_path_buf(),
            size(80, 24),
        );
        #[cfg(unix)]
        let request = command("while :; do read _; done", root.path());
        let (mut transport, reader) = PtyTransport::spawn(request).expect("spawn");
        let process_id = transport.process_id();
        let output = drain(reader);
        transport.terminate().expect("terminate tree");
        output.join().expect("reader endpoint closed");
        assert_process_stops(process_id);
    }
}

#[test]
fn primary_exit_reclaims_a_surviving_descendant() {
    let root = tempfile::tempdir().expect("root");
    let marker = root.path().join("orphan.pid");
    #[cfg(windows)]
    let script = format!(
        "$p=Start-Process powershell.exe -ArgumentList '-NoLogo','-NoProfile','-Command','Wait-Event' -PassThru; Set-Content -LiteralPath '{}' -Value $p.Id",
        marker.display()
    );
    #[cfg(unix)]
    let script = format!("sleep 300 & printf '%s' $! > '{}'", marker.display());
    let (mut transport, reader) =
        PtyTransport::spawn(command(&script, root.path())).expect("spawn");
    let output = drain(reader);
    let descendant = wait_for_pid_marker(&marker);
    let exit = transport.wait().expect("primary exit");
    output.join().expect("reader endpoint closed");
    assert!(exit.success, "{exit:?}");
    assert_process_stops(descendant);
}

#[test]
fn a_child_that_stops_reading_cannot_stall_another_session() {
    let root = tempfile::tempdir().expect("root");
    #[cfg(windows)]
    let blocked_request = command("Wait-Event", root.path());
    #[cfg(unix)]
    let blocked_request = command("exec tail -f /dev/null", root.path());
    let (mut blocked, blocked_reader) =
        PtyTransport::spawn(blocked_request).expect("blocked spawn");
    let blocked_output = drain(blocked_reader);
    blocked
        .writer()
        .write_bytes(&vec![b'x'; MAX_QUEUED_WRITE_BYTES])
        .expect("bounded bulk enqueue");

    #[cfg(windows)]
    let independent_request = SpawnRequest::new(
        ["cmd.exe", "/d", "/s", "/c", "echo KORU:INDEPENDENT"],
        root.path().to_path_buf(),
        size(80, 24),
    );
    #[cfg(unix)]
    let independent_request = command("printf 'KORU:INDEPENDENT\\n'", root.path());
    let (mut independent, independent_reader) =
        PtyTransport::spawn(independent_request).expect("independent spawn");
    let independent_output = drain(independent_reader);
    assert!(independent.wait().expect("independent exit").success);
    let independent_output = independent_output.join().expect("independent reader");
    assert!(
        independent_output.contains("KORU:INDEPENDENT"),
        "{independent_output:?}"
    );

    blocked.terminate().expect("blocked process cleanup");
    blocked_output.join().expect("blocked endpoint cleanup");
}

fn wait_for_pid_marker(marker: &Path) -> u32 {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(marker)
            && let Ok(pid) = text.trim().parse()
        {
            return pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "PID marker was not written"
        );
        std::thread::yield_now();
    }
}

#[cfg(windows)]
fn assert_process_stops(pid: u32) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let status = std::process::Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 1 }}"),
            ])
            .status()
            .expect("process probe");
        if status.success() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "descendant {pid} survived"
        );
        std::thread::yield_now();
    }
}

#[cfg(unix)]
fn assert_process_stops(pid: u32) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "descendant {pid} survived"
        );
        std::thread::yield_now();
    }
}
