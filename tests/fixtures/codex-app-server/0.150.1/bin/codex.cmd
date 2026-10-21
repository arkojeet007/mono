@echo off
rem Absolute path on purpose: under `cargo test` the harness PATH carries
rem every native build-script directory (59 whisper-rs-sys entries, ~13k
rem chars) and cmd.exe stops searching PATH past its 8191-character
rem expansion limit, so a bare `powershell.exe` is not found. The real npm
rem shim resolves via PATH; this fixture must work where the harness runs.
rem `--version` is answered here, without PowerShell. A session start asks
rem it with a 5 s limit, and on a CI runner with no GPU every Agent under
rem test is compiling its renderer on the software adapter at that moment;
rem a cold powershell.exe did not start inside the limit, and the run failed
rem as provider_unavailable. The POSIX fixture answers just as quickly.
if "%~1"=="--version" if "%~2"=="" (
    if defined CODEX_FIXTURE_TRACE (echo args:--version)>>"%CODEX_FIXTURE_TRACE%"
    echo codex-cli 0.150.1
    exit /b 0
)
rem The clock lines say where a slow start went: this one is written before
rem PowerShell is asked to start, the script's own once it is running.
if defined CODEX_FIXTURE_TRACE (echo clock:shim %TIME%)>>"%CODEX_FIXTURE_TRACE%"
%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%~dp0codex-fixture.ps1" %*
