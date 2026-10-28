@echo off
rem SPDX-License-Identifier: MPL-2.0

rem Task runner for Koru.
rem
rem   koru.bat run      build and launch (Koru Cloud settings from .env.production, if present)
rem   koru.bat run-all  restart Agent (with its API Gateway) and Koru, with the same
rem                     .env.production; --ai opens AI Chat, --profile gives only
rem                     the Koru workspace an explicit profile
rem   koru.bat stop-all stop this worktree's Koru, AI Chat and services
rem   koru.bat root     the same stack on one isolated root (.env.production too)
rem   koru.bat build-root-signed  build/register a signed rooted Windows test
rem   koru.bat run-root-signed    run the latest signed rooted Windows test
rem   koru.bat stop     stop every Koru process on this machine
rem   koru.bat test     run the test suite
rem   koru.bat envs     list every KORU_* environment variable and its purpose
rem   koru.bat setup-envs walk the runtime switches; choices persist in this cmd session
rem   koru.bat setup    set up sccache, jobs, fonts and third-party code
rem                     --check reports; --defender-exclusions and --agent-skills opt in
rem
rem Everything after the subcommand is forwarded to cargo, except the optional
rem path consumed by `root` and `root-stop`; the rooted signed verbs take none.
setlocal enabledelayedexpansion
cd /d "%~dp0"

set "SUB=%~1"
rem `root` and `root-stop` take one script argument rather than cargo args.
rem Capture it before `shift`; the general collector below deliberately loses
rem argument positions because every other verb forwards the whole tail.
set "ROOT_COMMAND_PATH=%~2"
set "ROOT_COMMAND_EXTRA=%~3"
if "%SUB%"=="" set "SUB=run"
rem `--window-size` is an app flag, not a verb. Leave it in the arg list so
rem `koru.bat --window-size 1024x768` is the same launch as `koru.bat run -- --window-size 1024x768`.
if /i "!SUB!"=="--window-size" (
  set "SUB=run"
) else if /i "!SUB:~0,14!"=="--window-size=" (
  set "SUB=run"
) else if not "%~1"=="" (
  shift
)

rem Collect the remaining arguments so they can be forwarded to cargo. `shift`
rem does not update %*, so they have to be gathered one at a time.
set "ARGS="
set "AFTER_CARGO_SEPARATOR="
set "RUN_RELEASE="
set "NEED_APP_SEP="
:collect
if "%~1"=="" goto dispatch
if not defined AFTER_CARGO_SEPARATOR if /i "%~1"=="--release" set "RUN_RELEASE=1"
if "%~1"=="--" set "AFTER_CARGO_SEPARATOR=1"
if not defined AFTER_CARGO_SEPARATOR (
  if /i "%~1"=="--window-size" set "NEED_APP_SEP=1"
  set "COLLECT_ARG=%~1"
  if /i "!COLLECT_ARG:~0,14!"=="--window-size=" set "NEED_APP_SEP=1"
)
set "ARGS=!ARGS! %1"
shift
goto collect

:dispatch
rem `setup` is the verb that tells you sccache is missing, so it runs before
rem the gate below rather than being refused by it.
if /i "%SUB%"=="setup" goto do_setup

rem **sccache is required to build, so say so here rather than in cargo's voice.**
rem `.cargo/config.toml` sets `rustc-wrapper = "sccache"` and there is no
rem optional form of that: without it on PATH every cargo command fails with an
rem error about a missing program, which reads as a broken checkout rather than
rem as a missing tool. One check, once, with the command that fixes it.
where sccache >nul 2>&1
if errorlevel 1 (
  echo koru: sccache is not on PATH, and this repository builds through it.
  echo.
  echo   winget install Mozilla.sccache
  echo.
  echo Every worktree shares one compiler cache, which is what makes a second
  echo worktree cheap to build. See .cargo\config.toml, and koru.bat setup.
  exit /b 1
)

rem **Three crates are built from patched source that is fetched, not committed**
rem (third-party\README.md), and the workspace names them by path, so every
rem verb that reaches cargo needs them first. `help` does not; nothing else here
rem is worth a special case.
if /i not "%SUB%"=="help" if /i not "%SUB%"=="-h" if /i not "%SUB%"=="--help" (
  call :ensure_third_party || exit /b 1
)

rem One channel root for every verb that talks to this stack.
rem
rem **Opt-in, and deliberately not the default.** Pointing a dev build at its own
rem store is the right habit - it is what would have stopped a schema bump in one
rem worktree from locking every other build out of the shared database - but
rem switching it on silently would make ten real workspaces disappear from a build
rem that had been showing them, which is indistinguishable from data loss to the
rem person looking at it. Set KORU_ROOT and it applies to run, jank,
rem voice and agent together; that agreement is the whole value, because the endpoint
rem those processes meet on carries a digest of the root, so a mismatched
rem spelling finds no store at all rather than the wrong one.
rem
rem   set KORU_ROOT=%CD%\target\koru-data
rem
rem **Below the label, not above it.** `:collect` ends in `goto dispatch`, so
rem anything between the two is jumped over and never runs - which is exactly how
rem this was written the first time, and it looked like the variable was being
rem ignored rather than like unreachable code.
rem
rem **Nothing changes when KORU_ROOT is unset**, and that is the whole reason
rem there are two variables rather than one. "run" forwards its arguments to
rem cargo, so "koru.bat run --release" must stay a cargo flag; injecting a "--"
rem unconditionally turned it into an argument for the app, which the strict
rem parser then refuses. ROOTSEP carries the separator for the verbs that have
rem none, ROOTONLY is for the verbs that already write their own, and both are
rem empty without a root.
rem
rem PASS strips a caller-supplied " -- " so it is not emitted twice. The test is
rem four characters wide on purpose: " -- " is a separator, " --release" is a
rem cargo flag, and a three-character test would eat the second one.
rem
rem KORU_ROOT names a *channel* root - the directory holding store\ and agent\
rem beside each other. Product services, including the Gateway, receive it whole.
rem
rem There used to be a third variable, GATEWAYONLY, spelled exactly like
rem ROOTONLY and shared by the gateway and voice verbs. The gateway verb went
rem with its executable and the duplicate went with it; the voice verb uses
rem ROOTONLY, which is the same string it was always handed.
set "ROOTONLY="
set "ROOTSEP="
set "PASS=!ARGS!"
if defined KORU_ROOT (
  set "ROOTONLY= --root "%KORU_ROOT%""
  set "ROOTSEP= -- --root "%KORU_ROOT%""
  if "!PASS:~0,4!"==" -- " set "PASS=!PASS:~3!"
  echo [Koru] channel root: %KORU_ROOT%
)
rem `--window-size` belongs to the app. Cargo treats unknown `--*` as its own
rem unless `--` separates them; inject that when the caller omitted it and
rem KORU_ROOT has not already written one.
if defined NEED_APP_SEP if not defined ROOTSEP set "ROOTSEP= --"

if /i "%SUB%"=="run"     goto do_run
if /i "%SUB%"=="run-all" goto do_run_all
if /i "%SUB%"=="stop-all" goto do_stop_all
if /i "%SUB%"=="jank"    goto do_jank
if /i "%SUB%"=="root"    goto do_root
if /i "%SUB%"=="root-open" goto do_root_open
if /i "%SUB%"=="root-stop" goto do_root_stop
if /i "%SUB%"=="build-root-signed" goto do_build_root_signed
if /i "%SUB%"=="run-root-signed" goto do_run_root_signed
if /i "%SUB%"=="stop"    goto do_stop
if /i "%SUB%"=="release" goto do_release
if /i "%SUB%"=="mcp"      goto do_mcp
if /i "%SUB%"=="mcp-add"    goto do_mcp_add
if /i "%SUB%"=="mcp-remove" goto do_mcp_remove
if /i "%SUB%"=="voice"    goto do_voice
if /i "%SUB%"=="agent"    goto do_agent
if /i "%SUB%"=="document" goto do_document
if /i "%SUB%"=="dist-local" goto do_dist_local
if /i "%SUB%"=="dist-cloud" goto do_dist_cloud
if /i "%SUB%"=="release-tool" goto do_release_tool
if /i "%SUB%"=="voice-models" goto do_voice_models
if /i "%SUB%"=="voice-test" goto do_voice_test
if /i "%SUB%"=="backup"  goto do_backup
if /i "%SUB%"=="build"   goto do_build
if /i "%SUB%"=="test"    goto do_test
if /i "%SUB%"=="fmt"     goto do_fmt
if /i "%SUB%"=="check"   goto do_check
if /i "%SUB%"=="fonts"   goto do_fonts
if /i "%SUB%"=="envs"       goto do_envs
if /i "%SUB%"=="setup-envs" goto do_setup_envs
if /i "%SUB%"=="setup"      goto do_setup
if /i "%SUB%"=="logo"    goto do_logo
if /i "%SUB%"=="clean"   goto do_clean
if /i "%SUB%"=="help"    goto do_help
if /i "%SUB%"=="-h"      goto do_help
if /i "%SUB%"=="--help"  goto do_help
echo koru: unknown command "%SUB%"
echo.
goto do_help

:do_envs
rem The table comes from the binary, not from this script: koru.exe --envs
rem prints crates/koru-env's registry, the same one reference/ENV.md is generated
rem from, so the listing cannot drift from what the code reads. The built exe
rem answers instantly; cargo only runs when nothing is built yet.
if exist "target\debug\koru.exe" (
    "target\debug\koru.exe" --envs
) else if exist "target\release\koru.exe" (
    "target\release\koru.exe" --envs
) else (
    cargo run -q -p koru-env
)
exit /b %ERRORLEVEL%

:do_setup
rem Set up whatever is not set up, and say what it could not do. The
rem third-party code here, then sccache, cache settings, cargo jobs and fonts.
rem Defender exclusions and agent skills/hooks require explicit flags. Run it again and
rem it does only what is still missing, retrying what failed. A fresh clone
rem needs nothing else before `run`. `--check` reports and changes nothing.
rem Every WARN prints the command that fixes it, and the exit code is the
rem number of them. README.md, "Build and run from a fresh clone", lists what
rem the machine needs.
set "SETUP_CHECK="
set "SETUP_DEFENDER="
set "SETUP_SKILLS="
for %%A in (%ARGS%) do (
  if /i "%%~A"=="--check" (
    set "SETUP_CHECK=-Check"
  ) else if /i "%%~A"=="--defender-exclusions" (
    set "SETUP_DEFENDER=-DefenderExclusions"
  ) else if /i "%%~A"=="--agent-skills" (
    set "SETUP_SKILLS=1"
  ) else (
    echo koru: unknown setup option "%%~A"
    echo usage: koru.bat setup [--check] [--defender-exclusions] [--agent-skills]
    exit /b 2
  )
)
if defined SETUP_CHECK (
  powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\setup\setup-check.ps1" -Check !SETUP_DEFENDER!
  exit /b !ERRORLEVEL!
)
call :ensure_third_party
if defined SETUP_SKILLS call :ensure_skills
call :ensure_docs
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\setup\setup-check.ps1" !SETUP_DEFENDER!
exit /b %ERRORLEVEL%

:do_setup_envs
rem Walk the runtime switches one by one. The answers must outlive this
rem script: the setlocal at the top of this file means nothing set here reaches the calling
rem session, so the choices accumulate in EXPORTS and the exit line below is
rem `endlocal & %%EXPORTS%%` - percent expansion happens when that line is
rem parsed, BEFORE endlocal executes, so the set commands run in the caller's
rem context. That is the one sanctioned way out of setlocal, and this verb is
rem the only one that uses it.
cargo build -q -p koru-env
if errorlevel 1 exit /b 1
rem The seed must be inert but must NOT be `rem` - rem swallows the rest of
rem the line when executed, `&` and all, which would silently discard every
rem choice. `ver >nul` runs and says nothing.
set "EXPORTS=ver >nul"
echo Walking the runtime switches. y = on, n = off (clears), s = keep as is.
echo.
for /f "usebackq tokens=1,2,* delims=	" %%A in (`cargo run -q -p koru-env -- --tsv`) do (
    if /i "%%B"=="switch" (
        set "CURRENT=unset"
        if defined %%A set "CURRENT=!%%A!"
        echo %%A   [currently: !CURRENT!]
        echo   %%C
        choice /c YNS /m "  on, off, or skip"
        if errorlevel 3 (
            echo   left alone.
        ) else if errorlevel 2 (
            set "EXPORTS=!EXPORTS! & set "%%A=""
            echo   will be cleared.
        ) else (
            set "EXPORTS=!EXPORTS! & set "%%A=1""
            echo   will be set to 1.
        )
        echo.
    )
)
echo Done. Applying to this cmd session...
endlocal & %EXPORTS%
exit /b 0

REM Which Cloud this launch talks to is a file, not a flag you can forget.
REM
REM The dangerous mistake is silent: a desktop pointed at the wrong API signs
REM in, enables Cloud sync and uploads somebody's pictures to it, and nothing
REM on screen says which one it reached. So the Cloud's settings come from one
REM gitignored file, .env.production, and `:load_env_file` prints that it read
REM it before a single value is set -- or, with no file, that this launch has
REM no Cloud.
:do_run
set "KORU_RUN_ENV_FILE=.env.production"
call :ensure_fonts || exit /b 1
call :load_env_file "%KORU_RUN_ENV_FILE%" || exit /b 1
call :announce_environment
if defined RUN_RELEASE (
  call :stop_exact_exe "%CD%\target\release\koru.exe" || exit /b 1
) else (
  call :stop_exact_exe "%CD%\target\debug\koru.exe" || exit /b 1
)
cargo run%ROOTSEP%!PASS!
exit /b %ERRORLEVEL%

REM Restart this worktree's debug stack in dependency order. --ai opens the
REM separately hosted chat after its gateway is ready.
REM PowerShell owns startup so roots containing spaces remain one argument, the
REM Agent can start hidden, its gateway worker starts with it, and readiness is
REM observed rather than guessed with a fixed sleep. The runner remains in this
REM console while either UI is open; closing both or pressing Ctrl+C stops the
REM whole exact-path stack.
REM The same file as `run`, for the same reason: the stack `run-all` brings up
REM signs in and syncs like `run` does, so it must be just as impossible to
REM point at the wrong Cloud without noticing. Without a file read here it
REM would run with whatever KORU_* the shell happened to hold -- usually
REM nothing, which means no Cloud at all, silently.
:do_run_all
set "KORU_RUN_ENV_FILE=.env.production"
set "RUN_ALL_MCP="
set "RUN_ALL_PERF="
set "RUN_ALL_AI="
set "RUN_ALL_PROFILE="
set "RUN_ALL_PROFILE_NEXT="
set "RUN_ALL_BAD="
for %%A in (%ARGS%) do (
  if defined RUN_ALL_PROFILE_NEXT (
    set "RUN_ALL_PROFILE= -Profile "%%~A""
    set "RUN_ALL_PROFILE_NEXT="
  ) else if /i "%%A"=="--mcp" (
    set "RUN_ALL_MCP= -Mcp"
  ) else if /i "%%A"=="--performance" (
    set "RUN_ALL_PERF= -Performance"
  ) else if /i "%%A"=="--ai" (
    set "RUN_ALL_AI= -Ai"
  ) else if /i "%%A"=="--profile" (
    set "RUN_ALL_PROFILE_NEXT=1"
  ) else (
    set "RUN_ALL_BAD=%%A"
  )
)
if defined RUN_ALL_PROFILE_NEXT set "RUN_ALL_BAD=--profile"
if defined RUN_ALL_BAD (
  echo koru: %SUB% accepts only --mcp, --performance, --ai and --profile ^<name^|id^|path^>
  echo usage: koru.bat %SUB% [--mcp] [--performance] [--ai] [--profile ^<name^|id^|path^>]
  exit /b 2
)
call :ensure_fonts || exit /b 1
call :load_env_file "%KORU_RUN_ENV_FILE%" || exit /b 1
call :ensure_voice_runtime || exit /b 1
call :announce_environment
call :stop_all_exact || exit /b 1
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\dev\run-all.ps1"%RUN_ALL_MCP%%RUN_ALL_PERF%%RUN_ALL_AI%%RUN_ALL_PROFILE%
exit /b %ERRORLEVEL%

REM Stop the supervised images under this checkout's target\debug and nothing else.
REM This is the worktree-scoped counterpart to the deliberately machine-wide
REM `stop` command below.
:do_stop_all
if not "%ARGS%"=="" (
  echo koru: stop-all takes no arguments
  echo usage: koru.bat stop-all
  exit /b 2
)
call :stop_all_exact || exit /b 1
echo [Koru] stopped this worktree's Koru, AI Chat, Agent - with its gateway and
echo        sessions workers - the voice worker, and any retired image still running.
exit /b 0

:do_release
call :ensure_fonts || exit /b 1
call :announce_environment
call :stop_exact_exe "%CD%\target\release\koru.exe" || exit /b 1
if defined NEED_APP_SEP (
  cargo run --release --%ARGS%
) else (
  cargo run --release%ARGS%
)
exit /b %ERRORLEVEL%

REM Same launch with the stall watch on, writing where it can be attached to a
REM report. A verb rather than "set these two variables" because the moment
REM somebody needs this is the moment they are annoyed, and the log is only
REM useful if it was already running when the freeze happened.
:do_jank
call :ensure_fonts || exit /b 1
set "KORU_JANK=1"
if not defined KORU_JANK_LOG set "KORU_JANK_LOG=%CD%\koru-jank.log"
call :announce_environment
echo [Koru] stalls -^> %KORU_JANK_LOG%
cargo run%ROOTSEP%!PASS!
exit /b %ERRORLEVEL%

REM One isolated development stack. PowerShell owns the process orchestration
REM because it can pass a root containing spaces as one argument, wait on the
REM two endpoints without a fixed sleep, and start the console-subsystem debug
REM Agent hidden. Keeping that in batch quoting is how `C:\Users\First Last`
REM eventually becomes two different roots.
REM The same .env.production as `run`, for the same reason: an isolated root
REM that quietly used a different Cloud from `run` is how somebody tests against
REM the wrong API for an afternoon. root-stack.ps1 prints which file it read.
:do_root
if not "%ROOT_COMMAND_EXTRA%"=="" (
  echo koru: root takes at most one path
  echo usage: koru.bat root [path]
  exit /b 2
)
call :ensure_fonts || exit /b 1
call :announce_environment
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\dev\root-stack.ps1" -Action Start -Root "%ROOT_COMMAND_PATH%"
exit /b %ERRORLEVEL%

:do_root_open
if not "%~3"=="" (
  echo koru: root-open takes at most one path
  echo usage: koru.bat root-open [path]
  exit /b 2
)
set "ROOT_COMMAND_PATH=%~2"
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\dev\root-stack.ps1" -Action Open -Root "%ROOT_COMMAND_PATH%"
exit /b %ERRORLEVEL%

:do_root_stop
if not "%ROOT_COMMAND_EXTRA%"=="" (
  echo koru: root-stop takes at most one path
  echo usage: koru.bat root-stop [path]
  exit /b 2
)
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\dev\root-stack.ps1" -Action Stop -Root "%ROOT_COMMAND_PATH%"
exit /b %ERRORLEVEL%

REM Build and register the signed sparse Windows identity used to prove real
REM notifications from an isolated root. The lower-level script records the
REM verified bundle and stable disposable root only after registration succeeds.
:do_build_root_signed
if not "%ARGS%"=="" (
  echo koru: build-root-signed takes no arguments
  echo usage: koru.bat build-root-signed
  exit /b 2
)
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\dev\reminder-dev.ps1" -Action Build -DataRoot "target\root-signed-data"
exit /b %ERRORLEVEL%

REM Launch the most recently verified signed bundle. It deliberately stays in
REM this console so the development terminal cannot reclaim its child Agents.
:do_run_root_signed
if not "%ARGS%"=="" (
  echo koru: run-root-signed takes no arguments
  echo usage: koru.bat run-root-signed
  exit /b 2
)
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\dev\reminder-dev.ps1" -Action Run
exit /b %ERRORLEVEL%

REM The deliberately broad escape hatch for development: unlike root-stop,
REM this is not scoped to one data root or one worktree. Close every app first
REM so it gets a chance to finish its normal shutdown, then the PowerShell
REM helper guarantees that stale app/service processes release their exe locks.
:do_stop
if not "%ARGS%"=="" (
  echo koru: stop takes no arguments
  echo usage: koru.bat stop
  exit /b 2
)
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\dev\stop-all.ps1"
exit /b %ERRORLEVEL%

:do_agent
REM The Agent runs in this console. A plain launch already goes to the
REM notification area; there is no second detached product path. Diagnostics
REM remain opt-in; starting it by hand must not quietly turn on a frame trace.
REM
REM **Kill a running one first** -- Windows locks the exe, and a second agent
REM would stand down at the bind anyway, which reads as a build that did nothing.
REM Match the complete executable path: other worktrees and installed builds
REM may have an agent with the same image name, and they are not this launch's.
call :stop_exact_exe "%CD%\target\debug\koru-agent.exe" || exit /b 1
call :announce_environment
cargo run -p koru-agent --%ROOTONLY%!PASS!
exit /b %ERRORLEVEL%

REM There is no "gateway" verb. The API Gateway is a worker role of
REM koru-agent.exe -- `koru-agent.exe --worker gateway` -- which reads a launch
REM bootstrap from its supervisor's stdin and refuses to serve without one, so
REM it cannot be run from a command line at all. Its `status`, `stop` and
REM `unfiled` commands were deleted with the standalone executable and became no
REM Agent command.
REM
REM What replaces each: `koru.bat agent` runs the Agent in this console and
REM supervises a gateway there, which is the foreground diagnostic this verb
REM used to offer; `koru-agent.exe status` prints a `gateway:` line, probed
REM against the gateway's own rendezvous; and stopping the Agent stops the
REM stack, because its workers are its children.

REM The voice worker is started on demand by Agent - the first time somebody
REM dictates - so a direct launch here is a diagnostic and nothing else. It
REM takes the store root, like the gateway does, because that is where its endpoint,
REM its logs and its model cache are resolved from.
:do_voice
call :announce_environment
echo %ARGS% | findstr /I /C:"status" /C:"stop" /C:"--help" /C:"-h" /C:"--version" /C:"-V" >nul
if not errorlevel 1 (
  if exist "target\debug\koru-voice-worker.exe" (
    "target\debug\koru-voice-worker.exe"%ROOTONLY%!PASS!
  ) else (
    cargo run -p koru-voice-worker --bin koru-voice-worker --%ROOTONLY%!PASS!
  )
  exit /b %ERRORLEVEL%
)
REM Stop this worktree exact image first - Windows locks a running executable,
REM and an Agent may have left one alive after the window that dictated closed.
call :stop_exact_exe "%CD%\target\debug\koru-voice-worker.exe" || exit /b 1
cargo run -p koru-voice-worker --bin koru-voice-worker --%ROOTONLY%!PASS!
exit /b %ERRORLEVEL%

REM MCP is owned by the client that launches it: unlike the gateway, a detached
REM stdio server has nobody on the other end of its pipe. The task-runner target
REM therefore checks discovery by default, while --console is the explicit
REM foreground server for protocol inspection and manual client testing.
REM
REM Stop this checkout's exact debug image before every MCP build. Windows
REM locks a running executable, and an MCP host may have left the previous
REM stdio child alive after its UI closed. Matching the complete path preserves
REM servers launched from another worktree or an installed Koru.
:do_mcp
call :stop_exact_exe "%CD%\target\debug\koru-mcp.exe" || exit /b 1
call :announce_environment
set "MCPPASS=!PASS:--console=!"
echo %ARGS% | findstr /I /C:"--console" >nul
if not errorlevel 1 (
  cargo run -p koru-mcp -- serve --stdio%ROOTONLY%!MCPPASS!
  exit /b !ERRORLEVEL!
)
if "%ARGS%"=="" (
  cargo run -p koru-mcp -- doctor%ROOTONLY%
  exit /b !ERRORLEVEL!
)
if /i "!MCPPASS:~0,7!"==" --root" (
  cargo run -p koru-mcp -- doctor!MCPPASS!%ROOTONLY%
  exit /b !ERRORLEVEL!
)
cargo run -p koru-mcp --!PASS!%ROOTONLY%
exit /b %ERRORLEVEL%

REM Register this checkout's bridge with the agent CLIs on PATH - Claude Code,
REM Codex and Gemini - under a name, "koru" unless one is given, so testing an
REM agent against a dev build is one verb rather than a path to type into
REM each. The debug bridge beside the other binaries, with the channel root
REM every other verb here uses, so the agent reaches the store the window
REM shows. A CLI that is not installed is skipped and said so; the verb fails
REM only when none of the three is there. `mcp-remove` undoes it.
REM
REM The build is the point: the registered path is this checkout's bridge, so
REM running the verb again after a change ships the change. A bridge an MCP
REM host still has running holds the exe lock, so it is stopped first, as the
REM `mcp` verb does; a session already open keeps its old child until it is
REM restarted or told to reconnect.
REM
REM **User scope where the CLI has one.** Claude's and Gemini's default scope
REM is the directory the command ran in, so a session started anywhere else -
REM a terminal inside Koru, another worktree - would not see the entry. Codex
REM keeps one global config. Grok, Kimi, Copilot and Muse are left alone: no
REM registration command of theirs is verified here, and each has its own MCP
REM settings to paste the Welcome page's command into.
REM
REM `call`, because the npm CLIs are .cmd shims and cmd.exe would otherwise
REM hand them this script's remaining lines.
:do_mcp_add
call :mcp_name || exit /b 1
call :stop_exact_exe "%CD%\target\debug\koru-mcp.exe" || exit /b 1
cargo build -p koru-mcp || exit /b 1
set "MCP_BRIDGE=%CD%\target\debug\koru-mcp.exe"
set "MCP_ANY="
where claude >nul 2>&1
if errorlevel 1 (
  echo [Koru] claude is not on PATH; skipped
) else (
  call claude mcp add --scope user %MCP_NAME% -- "%MCP_BRIDGE%" serve --stdio%ROOTONLY% && set "MCP_ANY=1"
)
where codex >nul 2>&1
if errorlevel 1 (
  echo [Koru] codex is not on PATH; skipped
) else (
  call codex mcp add %MCP_NAME% -- "%MCP_BRIDGE%" serve --stdio%ROOTONLY% && set "MCP_ANY=1"
)
where gemini >nul 2>&1
if errorlevel 1 (
  echo [Koru] gemini is not on PATH; skipped
) else (
  call gemini mcp add --scope user %MCP_NAME% "%MCP_BRIDGE%" -- serve --stdio%ROOTONLY% && set "MCP_ANY=1"
)
if not defined MCP_ANY (
  echo koru: none of claude, codex or gemini is on PATH; nothing was registered.
  exit /b 1
)
exit /b 0

:do_mcp_remove
call :mcp_name || exit /b 1
where claude >nul 2>&1
if not errorlevel 1 call claude mcp remove --scope user %MCP_NAME%
where codex >nul 2>&1
if not errorlevel 1 call codex mcp remove %MCP_NAME%
where gemini >nul 2>&1
if not errorlevel 1 call gemini mcp remove --scope user %MCP_NAME%
exit /b 0

REM The server name for the two verbs above: the first argument, or "koru".
:mcp_name
set "MCP_NAME=koru"
for /f "tokens=1" %%A in ("%ARGS%") do set "MCP_NAME=%%A"
exit /b 0

:do_document
call :ensure_fonts || exit /b 1
call :announce_environment
cargo run -p koru-document%ARGS%
exit /b %ERRORLEVEL%

REM The whole package - every shipped binary and all thirty fonts - staged and
REM zipped into dist\cloud\, optionally signed for a local rehearsal, and never
REM uploaded. It replaced
REM an older dist script, which built koru.exe alone into a layout the updater
REM refuses and that dies at launch now that koru.exe needs its gateway.
:do_dist_local
call :ensure_fonts || exit /b 1
call :ensure_voice_runtime || exit /b 1
cargo run --release -p koru-release -- dist-local%ARGS%
exit /b %ERRORLEVEL%

REM The same package, published to the update service: tagged, signed and
REM uploaded from a clean tree. It needs Koru Works' release credentials -
REM the release key under %LOCALAPPDATA%\koru\release\ and
REM CLOUDFLARE_API_TOKEN - which this file never reads.
:do_dist_cloud
call :ensure_fonts || exit /b 1
call :ensure_voice_runtime || exit /b 1
cargo run --release -p koru-release -- dist-cloud%ARGS%
exit /b %ERRORLEVEL%

REM Any koru-release subcommand. A dist-* one packages, so it gets the fonts
REM and the voice runtime dist-local gets; the others build nothing - they
REM read or write what the release origin serves - and get neither. The
REM subcommands that publish need release credentials this file never reads.
:do_release_tool
set "RELEASE_TOOL_SUB="
for /f "tokens=1" %%V in ("!ARGS!") do set "RELEASE_TOOL_SUB=%%~V"
if not defined RELEASE_TOOL_SUB (
  echo koru: release-tool needs a koru-release subcommand
  echo usage: koru.bat release-tool ^<subcommand^> [args...]
  exit /b 2
)
if /i "!RELEASE_TOOL_SUB:~0,5!"=="dist-" (
  call :ensure_fonts || exit /b 1
  call :ensure_voice_runtime || exit /b 1
)
cargo run --release -p koru-release --%ARGS%
exit /b %ERRORLEVEL%

REM No ensure_fonts either - it fetches somebody else's weights, repackages
REM them and publishes them. It writes to production only behind --upload.
:do_voice_models
cargo run --release -p koru-release -- voice-models%ARGS%
exit /b %ERRORLEVEL%

:do_voice_test
if not "%ARGS%"=="" (
  echo koru: voice-test takes no arguments
  exit /b 2
)
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\dev\voice-test.ps1"
exit /b %ERRORLEVEL%

rem Through the binary rather than reimplemented here, so it resolves the
rem profile folder with the same code a normal launch does. A script that
rem guessed would eventually back up the wrong folder, quietly.
:do_backup
cargo run --quiet -p koru -- --backup%ARGS%
exit /b %ERRORLEVEL%

:do_build
call :ensure_voice_runtime || exit /b 1
cargo build%ARGS%
exit /b %ERRORLEVEL%

REM **Both halves are capped in .cargo\config.toml, and neither is a flag here.**
REM The build jobs key caps rustc; the harness that runs the compiled tests has
REM its own pool and does not read that key, so it is capped separately by the
REM env block's RUST_TEST_THREADS, at eight. This file passes neither - a number
REM written at a call site is one two launchers have to keep in step and a
REM hand-typed cargo test never gets. An explicit -- --test-threads=N still wins
REM over the env var, so koru.bat test -- --test-threads=1 serialises a suite
REM you are debugging.
REM **The suite may leave an Agent and a gateway behind, and they lock the exe.**
REM A test that finds no Agent spawns one, and that Agent starts a gateway.
REM They outlive the run, so the *next* `test` cannot replace
REM target\debug\koru-agent.exe and cargo says only "Access is denied.
REM (os error 5)" — which reads as a permissions problem and is not one.
REM
REM So the run stops this worktree's images first, exactly as `:do_run` already
REM stops koru.exe before rebuilding it, and for the same reason: Windows locks
REM a running image, and a build that cannot replace one has failed before it
REM started. Worktree-scoped, so another checkout's stack is untouched.
REM
REM The message afterwards is for what this *cannot* reach — an installed Koru,
REM or a build from another worktree, holding something of ours.
:do_test
call :ensure_voice_runtime || exit /b 1
call :stop_all_exact >nul 2>&1
cargo test --workspace%ARGS%
set "TEST_RESULT=%ERRORLEVEL%"
if not "%TEST_RESULT%"=="0" (
  echo.
  echo [Koru] if that failed with "Access is denied" on a .exe rather than on a
  echo        test, a previous run left an Agent or gateway holding it:
  echo.
  echo          koru.bat stop-all
  echo.
)
exit /b %TEST_RESULT%

:do_fmt
cargo fmt --all%ARGS%
exit /b %ERRORLEVEL%

:do_check
call :ensure_voice_runtime || exit /b 1
cargo check --workspace%ARGS%
exit /b %ERRORLEVEL%

:do_fonts
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\setup\fetch-fonts.ps1"
exit /b %ERRORLEVEL%

rem Read one dotenv file into this invocation's environment.
rem
rem **Anything already set wins**, which is the same rule `Import-DotenvFile`
rem in scripts\dev\root-stack.ps1 applies: a value typed in the shell, or chosen
rem through `setup-envs`, is a deliberate one-off and a file that overrode it
rem would make that impossible. The `setlocal` at the top of this file keeps
rem each invocation separate, so two launches from one console cannot
rem contaminate each other.
rem
rem **No file is no Cloud, said in one line.** A checkout without a Cloud
rem configuration -- every community checkout -- starts with Koru Cloud not
rem configured, and the app says "Koru Cloud isn't set up in this build"
rem wherever sign-in would be. The Cloud's own names are cleared, so a stale
rem value left in this shell cannot quietly become the Cloud a launch with no
rem file talks to; that is the wrong-Cloud mistake the file switch exists to
rem prevent. With a file, everything below applies unchanged. An empty file is
rem still the explicit choice of no Cloud.
:load_env_file
if not exist "%CD%\%~1" (
  set "KORU_PUBLIC_API_ORIGIN="
  set "KORU_PUBLIC_API_ISSUER="
  set "KORU_PUBLIC_API_CREDENTIAL_ROLLOUT="
  echo [Koru] no %~1: starting with Koru Cloud not configured; CONTRIBUTING.md says how to set one up.
  exit /b 0
)
echo [Koru] configuration: %~1
rem `eol=#` drops comments, blank lines yield no tokens, and `tokens=1,*` keeps
rem everything after the first `=` intact -- which matters because one of these
rem values is a JSON policy full of quotes and colons.
rem
rem **A shell value that disagrees with the file is announced.** Shell-wins is
rem the right rule and it is also how somebody ends up on production with a
rem localhost support URL: `setup-envs` and a stray `set` both outlive the
rem command that wrote them, and the file then applies four of its five lines
rem in silence. An override is still honoured -- it is deliberate -- but it no
rem longer happens quietly. Identical values say nothing, or every launch would
rem carry noise nobody reads.
for /f "usebackq eol=# tokens=1,* delims==" %%A in ("%CD%\%~1") do (
  if not defined %%A (
    set "%%A=%%B"
  ) else (
    for /f "delims=" %%C in ("!%%A!") do if not "%%C"=="%%B" (
      echo [Koru] WARNING: this shell overrides %~1: %%A=%%C 1>&2
      echo [Koru]          the file says %%A=%%B; open a new terminal to use it. 1>&2
    )
  )
)
exit /b 0

rem Print configuration once, immediately before a Koru process is launched.
rem Values are useful for durations and paths, but credentials must never enter
rem the console or its redirected logs. Redacting by name also protects a
rem future KORU_* credential without requiring every caller to remember it.
rem
rem **Silent when nothing is set**, which is the ordinary case. A line saying
rem the environment is empty is one a reader could have predicted without
rem reading it, printed above every single launch: report news, not activity.
rem
rem The scratch names are cleared on the way out: they begin with KORU_, so
rem leaving them set would have this routine reporting its own working state
rem as configuration, and would pass it down to the process being launched.
:announce_environment
for /f "tokens=1,* delims==" %%A in ('set KORU_ 2^>nul') do (
  set "KORU_ENV_NAME=%%A"
  set "KORU_ENV_VALUE=%%B"
  call :announce_environment_one
)
set "KORU_ENV_NAME="
set "KORU_ENV_VALUE="
set "KORU_ENV_DISPLAY="
exit /b 0

:announce_environment_one
set "KORU_ENV_DISPLAY=%KORU_ENV_VALUE%"
if /i not "%KORU_ENV_NAME:TOKEN=%"=="%KORU_ENV_NAME%" set "KORU_ENV_DISPLAY=^<redacted^>"
if /i not "%KORU_ENV_NAME:SECRET=%"=="%KORU_ENV_NAME%" set "KORU_ENV_DISPLAY=^<redacted^>"
if /i not "%KORU_ENV_NAME:PASSWORD=%"=="%KORU_ENV_NAME%" set "KORU_ENV_DISPLAY=^<redacted^>"
if /i not "%KORU_ENV_NAME:KEY=%"=="%KORU_ENV_NAME%" set "KORU_ENV_DISPLAY=^<redacted^>"
echo [Koru] environment: %KORU_ENV_NAME%=%KORU_ENV_DISPLAY%
exit /b 0

rem Stop only the process image this checkout is about to replace. A filename
rem match is deliberately insufficient: several worktrees, debug/release builds
rem and an installed Koru can all be running on one machine at the same time.
:stop_exact_exe
if not exist "%~1" exit /b 0
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\dev\stop-exact-exe.ps1" -ExecutablePath "%~1"
exit /b %ERRORLEVEL%

rem App first so it cannot reconnect while its services are coming down, then
rem Agent -- which takes its gateway and sessions workers with it. One helper
rem owns every path so a supervisor cannot recreate an earlier process between
rem independent invocations.
:stop_all_exact
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\dev\stop-worktree.ps1"
if errorlevel 1 exit /b 1
exit /b 0

rem Swap the logo from assets/logo.png end-to-end:
rem   1. rebuild assets/koru.ico (the taskbar/Explorer icon) from logo.png
rem   2. rebuild koru.exe so the new .ico is embedded into it
rem   3. clear the Windows taskbar icon cache and restart explorer
rem Step 3 is not optional. The app already re-points a pinned shortcut at the
rem current exe on every launch (refresh_pinned_shortcuts, apps/koru/src/
rem taskbar.rs), but that does not invalidate the iconcache_*.db files in
rem %LOCALAPPDATA%\Microsoft\Windows\Explorer\, which hold the actual taskbar
rem bitmaps and keep serving the OLD icon. IconCache.db is just a legacy stub --
rem deleting it alone does nothing. The real caches are iconcache_*.db, locked
rem while explorer runs, so explorer is stopped first and restarted after. The
rem cache clear runs after the rebuild so explorer re-caches the NEW exe. Needs
rem Python + Pillow for the .ico step; close a running koru.exe first (Windows
rem locks the exe and blocks the rebuild).
:do_logo
py -3 "scripts\icons\set-logo.py"
if errorlevel 1 exit /b 1
echo [logo] embedding the new icon into koru.exe...
cargo build -p koru
if errorlevel 1 (
    echo [logo] build failed -- close any running koru.exe first, then retry.
    exit /b 1
)
echo [logo] clearing the taskbar icon cache -- explorer will restart...
taskkill /f /im explorer.exe >nul 2>&1
del /a /f /q "%LOCALAPPDATA%\IconCache.db" >nul 2>&1
del /a /f /q "%LOCALAPPDATA%\Microsoft\Windows\Explorer\iconcache_*.db" >nul 2>&1
start explorer.exe
echo [logo] done. Launch with koru.bat run.
exit /b 0

:do_clean
cargo clean%ARGS%
exit /b %ERRORLEVEL%

:do_help
echo Koru - workspace editor
echo.
echo usage: koru.bat ^<command^> [args...]
echo.
echo   run        launch the manager: documents in one process
echo   run-all    restart Agent, Gateway and Koru against Koru Cloud (.env.production);
echo              --ai opens AI Chat; --mcp checks MCP
echo              --profile ^<name^|id^|path^> gives the Koru workspace an explicit profile
echo              --performance runs the stall watch (capture-stalls' env plus
echo              KORU_JANK) and logs Koru's output for a stall summary on exit
echo              run, run-all and root with no .env.production start with Koru Cloud not configured
echo   stop-all   stop this worktree's Koru, AI Chat, Agent and services
echo   jank       ...with the stall watch on, logging to koru-jank.log
echo   root       build and start Agent + API Gateway + Koru on one isolated root;
echo              its interactive Agent registers real global shortcuts
echo   root-open  open another window through an already-running isolated Agent
echo   root-stop  stop that root's Agent and services (close windows first)
echo   build-root-signed  build/register a signed rooted Windows notification test
echo   run-root-signed    launch the latest signed rooted Windows notification test
echo   stop       stop every Koru process on this machine
echo   release    launch an optimised build
echo   mcp        check MCP discovery; --console serves stdio in this console
echo   mcp-add    register this checkout's bridge with the agent CLIs on PATH
echo              (Claude Code, Codex, Gemini): mcp-add [name]; absent ones are skipped
echo   mcp-remove unregister it from the same: mcp-remove [name]; the name defaults to koru
echo   voice      run or inspect the voice worker (status ^| stop)
echo   agent      run the Koru Agent in this console; IPC tracing is opt-in
echo   document   launch the built-in document on its own
echo   dist-local package the whole bundle into dist\cloud\ - optional local signing, no upload
echo   dist-cloud publish stable, beta or nightly to the update service ^(release credentials^)
echo   release-tool run any koru-release subcommand; dist-* ones get the fonts
echo              and voice runtime first
echo   voice-models fetch, repackage and publish the speech models (dry run by default)
echo   voice-test  launch an isolated worktree stack with both local speech models
echo   backup     copy your workspace somewhere safe, right now
echo   build      compile without running
echo   test       run the test suite
echo   fmt        format the source
echo   check      type-check only
echo   fonts      download the bundled fonts into assets\fonts\
echo   envs       list every KORU_* environment variable and its purpose
echo   setup      set up whatever is not: sccache on the fastest drive and its cache
echo              size, cargo jobs, fonts and third-party code
echo   setup --check  report the same, change nothing
echo   setup --defender-exclusions  optional faster builds in a trusted checkout;
echo              disables scanning of source, dependency and cache directories
echo   setup --agent-skills  install coding-agent skills and their worktree hook
echo   setup-envs walk the runtime switches one by one; choices persist in this cmd session
echo   logo       swap the logo: rebuild icon + exe + taskbar cache
echo   clean      remove build artifacts
echo   help       this message
echo.
echo "run" is almost always the one you want: the manager shows both kinds of
echo view, so opening a document from a pill is instant and shares one undo
echo history. The single-view commands are separate processes, so two of them
echo are two replicas.
echo.
echo A profile lives in a folder you choose. The dev build keeps its own profile
echo list beside the exe under target\, so it stays separate from a released
echo build's. To open real content: koru.bat run -- --profile ^<path^>
echo To open at a size: koru.bat --window-size 1024x768
echo.
echo "dist-local" builds exactly what a published release carries - koru.exe,
echo koru-ai-chat.exe, koru-agent.exe, koru-com-notification.exe, koru-mcp.exe,
echo koru-voice-worker.exe, koru-updater.exe and all thirty
echo fonts - and
echo stops. It takes a channel and an optional version. Internal is local-only:
echo   stable ^| beta ^| nightly ^| internal
echo   koru.bat dist-local beta 1.0.0-beta.0
echo.
echo First clone? Just run "koru.bat run" - it fetches the fonts for you.
echo.
echo Isolated stack: "koru.bat root" uses .dev-data\root-mode. Set KORU_ROOT or
echo pass one path to override it; use the exact same path with "root-stop".
echo.
echo Diagnostics: scripts\dev\capture-stalls.ps1 records a stall trace, and
echo "python scripts\dev\read-events.py" reads a build's events journal read-only.
exit /b 0

rem Fetch the third-party code when the pins or patches changed. The stamp is a
rem byte copy of the manifest, so the common case is one `fc` and no PowerShell
rem start-up.
:ensure_third_party
fc /b "third-party\fetch.manifest" "third-party\crates\.fetched" >nul 2>&1 && exit /b 0
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\setup\fetch-third-party.ps1"
exit /b %ERRORLEVEL%

rem Install the agent skills (koruworks/skills, through the skills CLI) into
rem .agents\skills when no skill is there, link .claude\skills to it when that
rem link is missing or broken, and point core.hooksPath at .githooks when
rem nothing else has, so `git worktree add` does the same in each new worktree.
rem The link is a directory symlink where Windows allows one (Developer Mode),
rem because `git worktree remove` cannot delete a junction; a junction is the
rem fallback. The batch twin of scripts/setup/setup-skills.sh: keep the source, the
rem agent, the CLI version and the link in step. Never fails a verb; a missing
rem skill set is not a broken build.
:ensure_skills
git rev-parse --is-inside-work-tree >nul 2>&1 || exit /b 0
set "KORU_HOOKS_PATH="
for /f "delims=" %%H in ('git config --get core.hooksPath 2^>nul') do set "KORU_HOOKS_PATH=%%H"
if not defined KORU_HOOKS_PATH (
  git config core.hooksPath .githooks
  echo [Koru] core.hooksPath is now .githooks, so a new worktree installs the agent skills
)
rem A directory that exists is not enough: an emptied one must reinstall.
set "KORU_SKILLS="
for /d %%D in (".agents\skills\*") do if exist "%%D\SKILL.md" set "KORU_SKILLS=1"
if not defined KORU_SKILLS (
  where npx >nul 2>&1 || (
    echo [Koru] the agent skills are not installed: npx was not found. Install Node.js and run any verb again.
    exit /b 0
  )
  echo [Koru] installing the agent skills from koruworks/skills
  call npx -y skills@1.7.2 add koruworks/skills --agent codex --yes >nul 2>&1
  if errorlevel 1 (
    echo [Koru] the skills install failed; run: npx skills add koruworks/skills
    exit /b 0
  )
  echo [Koru] agent skills installed in .agents\skills
  rem Record the published commit, for the session-start check to compare against.
  call ".agents\check-skills.bat" --record
)
rem The link: present as a link (a reparse point), and showing the skills.
set "KORU_LINK="
dir /AL /B ".claude" 2>nul | findstr /X /I "skills" >nul && set "KORU_LINK=1"
set "KORU_LINKED="
if defined KORU_LINK for /d %%D in (".claude\skills\*") do if exist "%%D\SKILL.md" set "KORU_LINKED=1"
if defined KORU_LINKED exit /b 0
rem A link is removed as a link (rmdir without /s); copies from an older
rem install are a real directory and go whole.
if defined KORU_LINK (
  rmdir ".claude\skills"
) else if exist ".claude\skills\" (
  rmdir /s /q ".claude\skills"
)
if not exist ".claude\" mkdir ".claude"
mklink /D ".claude\skills" "..\.agents\skills" >nul 2>&1 && (
  echo [Koru] .claude\skills links to .agents\skills
  exit /b 0
)
mklink /J ".claude\skills" ".agents\skills" >nul 2>&1 && (
  echo [Koru] .claude\skills is a junction to .agents\skills: Windows would not make a symlink
  echo [Koru]   without Developer Mode, and git worktree remove leaves a junction's worktree behind
  exit /b 0
)
echo [Koru] could not link .claude\skills to .agents\skills
exit /b 0

rem docs\ is the team's private koru-docs submodule. Nothing builds from it -
rem the references are in reference\ and the licensing audit in licensing\ -
rem so only `setup` tries to check it out, once, with credential prompts off:
rem without access every verb would otherwise raise a login window. A failure
rem is a note. Keep in step with scripts\setup\setup-docs.sh.
:ensure_docs
findstr /c:"path = docs" .gitmodules >nul 2>&1 || exit /b 0
if exist "docs\README.md" exit /b 0
set "GIT_TERMINAL_PROMPT=0"
set "GCM_INTERACTIVE=never"
git -c credential.interactive=never submodule update --init docs >nul 2>&1
if errorlevel 1 (
  echo [Koru] docs\ is the team's private koru-docs repository and could not be checked out;
  echo [Koru]   the build does not need it. With access: git submodule update --init docs
  exit /b 0
)
echo [Koru] checked out docs\ ^(koru-docs^)
exit /b 0

rem The app runs without the fonts (it falls back to system faces), but the first
rem run should look the way it is meant to, so fetch them if they are absent.
:ensure_fonts
rem A sentinel from each group the fetch script writes: a text family, its
rem italic (a separate file and a separate download), the CJK face, the icons,
rem the icons' Fill face, and the code family. One of each catches a
rem half-finished fetch without listing all thirty.
rem
rem The last two joined the list when packaging started requiring the whole set:
rem `package::build` refuses a bundle short of a face, and Phosphor-Fill and
rem JetBrainsMono are the two a tree fetched before they were added would be
rem missing - so a sentinel list that stopped at Phosphor.ttf said the fetch was
rem complete and left `dist-local` to report the failure instead.
if exist "assets\fonts\Inter.ttf" if exist "assets\fonts\Inter-Italic.ttf" if exist "assets\fonts\NotoSansKR-Regular.ttf" if exist "assets\fonts\Phosphor.ttf" if exist "assets\fonts\Phosphor-Fill.ttf" if exist "assets\fonts\JetBrainsMono.ttf" if exist "assets\fonts\JetBrainsMono-Italic.ttf" exit /b 0
echo [Koru] getting fonts (copy from sibling tree or download)...
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\setup\fetch-fonts.ps1"
exit /b %ERRORLEVEL%

:ensure_voice_runtime
if not defined SHERPA_ONNX_ARCHIVE_DIR set "SHERPA_ONNX_ARCHIVE_DIR=%CD%\third-party\sherpa-onnx\1.13.6"
powershell -NoProfile -ExecutionPolicy Bypass -File "scripts\setup\fetch-voice-runtime.ps1"
if errorlevel 1 exit /b 1
where cmake >nul 2>&1 || (
  echo [Koru] voice build needs CMake. Install CMake, then retry.
  exit /b 1
)
if defined LIBCLANG_PATH if exist "%LIBCLANG_PATH%\libclang.dll" exit /b 0
if exist "%ProgramFiles%\LLVM\bin\libclang.dll" (
  set "LIBCLANG_PATH=%ProgramFiles%\LLVM\bin"
  exit /b 0
)
echo [Koru] voice build needs LLVM/libclang. Install it once with:
echo.
echo   winget install LLVM.LLVM
echo.
echo Then open a new terminal and retry.
exit /b 1
