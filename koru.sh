#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0

# Task runner for Koru.
#
#   ./koru.sh run        build and launch (Koru Cloud settings from .env.production, if present)
#   ./koru.sh run-all    restart Agent and Koru, with the same .env.production
#   ./koru.sh test       run the test suite
#
# Everything after the subcommand is forwarded to cargo, so
# `./koru.sh test -- --nocapture` works as expected.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

# A sentinel from each group the fetch script writes: a text family (and its
# italic, which is a separate file and a separate download), the CJK face, the
# icons, the icons' Fill face, and the code family. Naming one of each catches a
# half-finished fetch without listing all thirty.
#
# The last two joined the list when packaging started requiring the whole set:
# `package::build` refuses a bundle short of a face, and Phosphor-Fill and
# JetBrainsMono are exactly the two that a tree fetched before they were added
# would be missing — so a sentinel list stopping at Phosphor.ttf reported the
# fetch as complete and left `dist-local` to discover otherwise.
FONTS=(
  assets/fonts/Inter.ttf
  assets/fonts/Inter-Italic.ttf
  assets/fonts/NotoSansKR-Regular.ttf
  assets/fonts/Phosphor.ttf
  assets/fonts/Phosphor-Fill.ttf
  assets/fonts/JetBrainsMono.ttf
  assets/fonts/JetBrainsMono-Italic.ttf
)

# The app runs fine without the fonts — it falls back to system faces — but the
# first run should look the way it is meant to, so fetch them if they are absent.
ensure_fonts() {
  for f in "${FONTS[@]}"; do
    if [ ! -s "$f" ]; then
      echo "[Koru] getting fonts (copy from sibling tree or download)..."
      bash scripts/setup/fetch-fonts.sh
      return
    fi
  done
}

# Set up this machine and checkout for the repository: make each fix that can
# be made safely, and print the command for each one that cannot. Run it again
# and it does only what is still missing, retrying what failed; a fresh clone
# needs nothing else before `run`. `--check` reports and changes nothing. The
# Windows twin also ranks drives and offers opt-in Defender exclusions;
# here the drive question is left to the person, because there is
# no portable answer to "which disk is fastest".
#
# What it changes: it installs sccache (Homebrew, or `cargo install`),
# writes SCCACHE_DIR (only when unset, at sccache's own default, so the cache
# is kept), SCCACHE_CACHE_SIZE and CARGO_BUILD_JOBS into one marked block of
# your shell profile, restarts the sccache server, fetches the fonts and the
# third-party code. --agent-skills also installs skills and their Git hook.
# Nothing is changed for a check that passes.
SETUP_BLOCK_BEGIN="# >>> koru setup >>>"
SETUP_BLOCK_END="# <<< koru setup <<<"

setup_profile() {
  case "${SHELL:-}" in
    */zsh) echo "$HOME/.zshrc" ;;
    */bash) if [ "$(uname -s)" = Darwin ]; then echo "$HOME/.bash_profile"; else echo "$HOME/.bashrc"; fi ;;
    *) echo "$HOME/.profile" ;;
  esac
}

# Set KEY=VALUE in this shell and in the marked block of the profile, keeping
# every other line of the block and of the file as it was.
setup_persist() {
  local key="$1" value="$2" profile tmp
  profile="$(setup_profile)"
  export "$key=$value"
  touch "$profile"
  tmp="$(mktemp)"
  awk -v begin="$SETUP_BLOCK_BEGIN" -v end="$SETUP_BLOCK_END" -v key="$key" -v line="export $key=\"$value\"" '
    $0 == begin { inside = 1; found = 1; print; next }
    inside && $0 == end { print line; print; inside = 0; next }
    inside && index($0, "export " key "=") == 1 { next }
    { print }
    END { if (!found) { print ""; print begin; print line; print end } }
  ' "$profile" >"$tmp" && cat "$tmp" >"$profile"
  rm -f "$tmp"
}

setup_verb() {
  local apply=1 agent_skills=0 option
  for option in "$@"; do
    case "$option" in
      --check) apply=0 ;;
      --agent-skills) agent_skills=1 ;;
      *) echo "koru: unknown setup option: $option" >&2
         echo "usage: ./koru.sh setup [--check] [--agent-skills]" >&2
         return 2 ;;
    esac
  done
  warnings=0
  fixes=0
  restart_server=0
  # Each ends in `return 0`: under `set -e` a helper whose last command is a
  # failed `[ -n ]` (no detail line) would otherwise end the whole check.
  ok()    { printf '  ok     %s\n' "$1"; [ -n "${2:-}" ] && printf '         %s\n' "$2"; return 0; }
  fixed() { fixes=$((fixes + 1)); printf '  fixed  %s\n' "$1"; [ -n "${2:-}" ] && printf '         %s\n' "$2"; return 0; }
  note()  { printf '  note   %s\n' "$1"; [ -n "${2:-}" ] && printf '         %s\n' "$2"; return 0; }
  warn()  { warnings=$((warnings + 1)); printf '  WARN   %s\n' "$1"; [ -n "${2:-}" ] && printf '         %s\n' "$2"; shift 2; for f in "$@"; do printf '         > %s\n' "$f"; done; return 0; }
  echo; echo "Koru setup check for $PWD"; echo
  if [ "$apply" = 1 ]; then
    bash scripts/setup/fetch-third-party.sh || true
    if [ "$agent_skills" = 1 ]; then sh scripts/setup/setup-skills.sh || true; fi
    sh scripts/setup/setup-docs.sh || true
    echo
  fi
  local profile
  profile="$(setup_profile)"
  # Every fetch script downloads with curl and has no fallback, and cargo can do
  # nothing until fetch-third-party.sh has filled third-party/crates/.
  if command -v curl >/dev/null 2>&1; then
    ok "curl is on PATH" "$(command -v curl)"
  else
    warn "curl is not on PATH" "The fetch scripts download with it, and the first build cannot start without them." \
      "sudo apt install curl   # Debian/Ubuntu;  macOS ships it"
  fi
  if command -v sccache >/dev/null 2>&1; then
    ok "sccache is on PATH" "$(command -v sccache)"
  elif [ "$apply" = 1 ] && command -v brew >/dev/null 2>&1 && brew install sccache; then
    fixed "sccache installed with Homebrew" "$(command -v sccache || true)"
  elif [ "$apply" = 1 ] && command -v cargo >/dev/null 2>&1 && RUSTC_WRAPPER= cargo install sccache --locked; then
    fixed "sccache installed with cargo install" "$(command -v sccache || true)"
  else
    warn "sccache is not on PATH" "Every cargo command here runs through it; there is no optional form." \
      "brew install sccache   # macOS;  or: cargo install sccache"
  fi
  if [ -n "${SCCACHE_DIR:-}" ]; then
    ok "SCCACHE_DIR is $SCCACHE_DIR" "Keep it on the fastest disk you have; every cache hit is a file read."
  elif [ "$apply" = 1 ]; then
    # sccache's own default, made explicit: the cache already there is kept.
    if [ "$(uname -s)" = Darwin ]; then dir="$HOME/Library/Caches/Mozilla.sccache"; else dir="${XDG_CACHE_HOME:-$HOME/.cache}/sccache"; fi
    mkdir -p "$dir"
    setup_persist SCCACHE_DIR "$dir"
    restart_server=1
    fixed "SCCACHE_DIR is now $dir, in $profile" "sccache's default location, so the cache is kept; move it if a faster disk exists."
  else
    warn "SCCACHE_DIR is not set, so the cache sits at sccache's default (~/Library/Caches/Mozilla.sccache or ~/.cache/sccache)" \
      "Put it on the fastest disk you have, then restart the server." \
      'export SCCACHE_DIR="$HOME/sccache/cache"   # in your shell profile' \
      "sccache --stop-server"
  fi
  if [ -n "${SCCACHE_CACHE_SIZE:-}" ]; then
    ok "SCCACHE_CACHE_SIZE is $SCCACHE_CACHE_SIZE"
  elif [ "$apply" = 1 ]; then
    setup_persist SCCACHE_CACHE_SIZE 100G
    restart_server=1
    fixed "SCCACHE_CACHE_SIZE is now 100G, in $profile" "Up from sccache's 10 GiB default, which this workspace outgrows."
  else
    warn "SCCACHE_CACHE_SIZE is not set, so the ceiling is sccache's 10 GiB default" \
      "One workspace build across two worktrees is more than that; the cache evicts and recompiles what it just cached." \
      'export SCCACHE_CACHE_SIZE=100G   # in your shell profile' \
      "sccache --stop-server"
  fi
  if command -v sccache >/dev/null 2>&1 && [ "$restart_server" = 0 ]; then
    sccache --show-stats 2>/dev/null | grep -E 'Max cache size|Cache location' | sed 's/^ */         sccache server: /'
  fi
  # Four GB per rustc is the working rule; see scripts/setup/setup-check.ps1.
  cores=$( (nproc 2>/dev/null || sysctl -n hw.logicalcpu 2>/dev/null || echo 8) )
  if [ "$(uname -s)" = Darwin ]; then
    ram_gib=$(( $(sysctl -n hw.memsize) / 1073741824 ))
  else
    ram_gib=$(( $(awk '/MemTotal/ {print $2}' /proc/meminfo 2>/dev/null || echo 0) / 1048576 ))
  fi
  recommended=$(( (ram_gib + 2) / 4 )); [ "$recommended" -gt "$cores" ] && recommended=$cores; [ "$recommended" -lt 2 ] && recommended=2
  configured=$(sed -n 's/^[[:space:]]*jobs[[:space:]]*=[[:space:]]*\([0-9][0-9]*\).*/\1/p' .cargo/config.toml | head -1)
  effective=${CARGO_BUILD_JOBS:-$configured}
  source_name=$([ -n "${CARGO_BUILD_JOBS:-}" ] && echo CARGO_BUILD_JOBS || echo .cargo/config.toml)
  machine="$cores logical cores, $ram_gib GB RAM"
  if [ -n "$effective" ] && [ "$effective" -gt "$recommended" ] && [ "$apply" = 1 ]; then
    setup_persist CARGO_BUILD_JOBS "$recommended"
    fixed "CARGO_BUILD_JOBS is now $recommended (was $effective, from $source_name), in $profile" \
      "For this machine ($machine); the tracked config stays as it is."
  elif [ -z "$effective" ]; then
    warn "cargo build jobs: nothing configured" "$recommended fits this machine ($machine)." "export CARGO_BUILD_JOBS=$recommended   # in your shell profile"
  elif [ "$effective" -gt "$recommended" ]; then
    warn "cargo build jobs = $effective ($source_name); $recommended fits this machine ($machine)" \
      "More jobs than memory allows is a build that takes the desktop with it." \
      "export CARGO_BUILD_JOBS=$recommended   # per machine; the tracked config stays as it is"
  elif [ "$effective" -lt "$recommended" ]; then
    ok "cargo build jobs = $effective ($source_name)" "$recommended would fit this machine ($machine); fewer is safe, only slower."
  else
    ok "cargo build jobs = $effective ($source_name) fits this machine ($machine)"
  fi
  fonts_missing() {
    missing=0
    for f in "${FONTS[@]}"; do [ -s "$f" ] || missing=$((missing + 1)); done
  }
  fonts_missing
  if [ "$missing" -eq 0 ]; then
    ok "bundled fonts are present in this worktree"
  elif [ "$apply" = 1 ] && { bash scripts/setup/fetch-fonts.sh >/dev/null 2>&1 || true; fonts_missing; [ "$missing" -eq 0 ]; }; then
    fixed "bundled fonts fetched into this worktree"
  else
    warn "bundled fonts: $missing of ${#FONTS[@]} missing from this worktree" \
      "They are gitignored and fetched per worktree; the font-shaping tests fail without them." "./koru.sh fonts"
  fi
  if [ -f third-party/crates/.fetched ] && cmp -s third-party/fetch.manifest third-party/crates/.fetched; then
    ok "third-party code is fetched and matches third-party/fetch.manifest"
  else
    warn "third-party code is not fetched, or is older than third-party/fetch.manifest" \
      "Every cargo command needs it; any ./koru.sh verb fetches it." "./koru.sh setup"
  fi
  if ls .agents/skills/*/SKILL.md >/dev/null 2>&1 && [ -L .claude/skills ] && ls .claude/skills/*/SKILL.md >/dev/null 2>&1; then
    ok "agent skills are installed (koruworks/skills), and .claude/skills links to them"
  elif ls .agents/skills/*/SKILL.md >/dev/null 2>&1; then
    note "Optional agent tooling needs repair: ./koru.sh setup --agent-skills"
  else
    note "Optional coding-agent skills and Git hooks: ./koru.sh setup --agent-skills (requires Node.js)"
  fi
  # docs/ is the team's private koru-docs submodule; nothing builds from it,
  # so not having it is a note, not a warning.
  if [ -f docs/README.md ]; then
    ok "docs/ is checked out (the koru-docs submodule)"
  else
    note "docs/ is not checked out: it is the team's private koru-docs repository" "The build does not need it. With access: git submodule update --init docs"
  fi
  hooks="$(git config --get core.hooksPath 2>/dev/null || true)"
  if [ "$hooks" = .githooks ]; then
    ok "core.hooksPath is .githooks, so a new worktree installs the skills"
  elif [ -n "$hooks" ]; then
    ok "core.hooksPath is $hooks" "Run setup --agent-skills in a new worktree if you want agent tooling."
  else
    note "Optional worktree skill installation: ./koru.sh setup --agent-skills"
  fi
  if [ "$restart_server" = 1 ] && command -v sccache >/dev/null 2>&1; then
    sccache --stop-server >/dev/null 2>&1 || true
    printf '         %s\n' "sccache server stopped; the next cargo command starts one with the new settings."
  fi
  echo
  if [ "$warnings" -eq 0 ] && [ "$fixes" -eq 0 ]; then
    echo "Nothing to change."
  elif [ "$warnings" -eq 0 ]; then
    echo "Set up $fixes thing(s); nothing left to change."
  elif [ "$apply" = 1 ]; then
    echo "$warnings thing(s) to change; each has its command above, and the next setup tries again."
  else
    echo "$warnings thing(s) to change; each has its command above, or run ./koru.sh setup."
  fi
  if [ "$apply" = 1 ] && grep -qF "$SETUP_BLOCK_BEGIN" "$profile" 2>/dev/null; then
    echo "Settings written to $profile; open a new terminal, or: source $profile"
  fi
  echo "README.md, \"Build and run from a fresh clone\", lists what this machine needs."
  exit "$warnings"
}

# `--window-size` is Koru's, not cargo's. Without a `--` separator cargo
# treats it as an unknown cargo flag and never starts the app.
needs_app_separator() {
  local arg sep=0 found=0
  for arg in "$@"; do
    [ "$arg" = -- ] && sep=1
    case "$arg" in
      --window-size|--window-size=*) found=1 ;;
    esac
  done
  [ "$found" = 1 ] && [ "$sep" = 0 ]
}

# **Linux does not build the voice worker yet.** It links both speech engines
# and cpal unconditionally, and neither engine's native runtime is provisioned
# on Linux until its voice rung (docs/features/linux/TECHNICAL.final.md, D4a).
# Agent no longer links it, so leaving it out costs a window nothing; the rung
# that brings Linux voice deletes this and its callers.
builds_voice_worker() {
  [ "$(uname -s)" != Linux ]
}

# What a workspace-wide cargo command leaves out on this platform.
WORKSPACE_EXCLUDES=()
builds_voice_worker || WORKSPACE_EXCLUDES=(--exclude koru-voice-worker)

ensure_voice_runtime() {
  # Status 3 is the script saying this platform has no voice runtime
  # provisioned (Linux until its voice rung). It has said so in one line;
  # nothing here will build voice, so the CMake and libclang checks below
  # would only fail a command that does not need them.
  local fetched=0
  bash scripts/setup/fetch-voice-runtime.sh || fetched=$?
  if [ "$fetched" = 3 ]; then
    return 0
  fi
  [ "$fetched" = 0 ] || return "$fetched"
  export SHERPA_ONNX_ARCHIVE_DIR="$PWD/third-party/sherpa-onnx/1.13.6"
  if ! command -v cmake >/dev/null 2>&1; then
    echo "[Koru] voice build needs CMake. Install it, then retry." >&2
    return 1
  fi
  if [ -n "${LIBCLANG_PATH:-}" ]; then
    return 0
  fi
  if command -v brew >/dev/null 2>&1; then
    local llvm_prefix
    llvm_prefix="$(brew --prefix llvm 2>/dev/null || true)"
    if [ -n "$llvm_prefix" ] && [ -d "$llvm_prefix/lib" ]; then
      export LIBCLANG_PATH="$llvm_prefix/lib"
      return 0
    fi
  fi
  echo "[Koru] voice build needs LLVM/libclang. Install LLVM, then retry." >&2
  return 1
}

usage() {
  cat <<'EOF'
Koru — workspace editor

usage: ./koru.sh <command> [cargo args...]

  run        launch the manager: documents in one process
  run-all    start Agent, API Gateway and Koru; --ai also opens Koru AI Chat
             (run and run-all read .env.production; with none, Koru Cloud is not configured)
  stop-all   stop this worktree's Agent, services, voice worker, Koru and AI Chat
  jank       ...with the stall watch on, logging to koru-jank.log
  release    launch an optimised build
  release-tool run any koru-release subcommand; dist-* ones get the fonts first
  voice-models fetch, repackage and publish the speech models (dry run by default)
  voice-test  launch an isolated stack with both locally served speech models
  mcp        check MCP discovery; --console serves stdio in this terminal
  voice      run or inspect the voice worker (status | stop)
  document   launch the built-in document on its own
  dist-local package the whole bundle into dist/cloud/ — local signing, no upload
  dist-cloud publish stable, beta or nightly to the update service (release credentials)
  dist-try   (mac) open the last dist-local artifact, quarantined like a download
  nightly-notes draft the next GitHub nightly's notes into releases/nightly/ (never commits)
  backup     copy your workspace somewhere safe, right now
  build      compile without running
  test       run the test suite
  fmt        format the source
  check      type-check only
  fonts      download the bundled fonts into assets/fonts/
  envs       list every KORU_* environment variable and its purpose
  setup      set up whatever is not: sccache and its cache size, cargo jobs, fonts,
             and third-party code; again later, it retries what failed
  setup --check  report the same, change nothing
  setup --agent-skills  install coding-agent skills and their worktree hook
  setup-envs walk the runtime switches; apply with: eval "$(./koru.sh setup-envs)"
  logo       swap the logo: rebuild icon + exe from assets/logo.png
  clean      remove build artifacts
  help       this message

`run` is almost always the one you want: the manager shows both kinds of view,
so opening a document from a pill is instant and shares one undo history. The
single-view commands are for working on one view without the other in the way —
each is a separate process, so two of them are two replicas.

Everything launched from here keeps its profile list beside the dev exe under
target/, so a build being worked on stays separate from a released build's. To
open real content from this tree, say so: `./koru.sh run -- --profile <path>`.
To open at a size: `./koru.sh --window-size 1024x768`.

`dist-local` builds exactly what a published release carries — on a Mac that is
a Koru.app bundle holding the six binaries, the notification owner and the
thirty fonts — and then
stops. It takes a channel and an optional version:
`./koru.sh dist-local beta 1.0.0-beta.0`. On macOS it signs through
`KORU_MAC_SIGN_COMMAND` and notarizes/staples through
`KORU_MAC_NOTARIZE_COMMAND`; without those hooks it produces an ad-hoc signed
local bundle. reference/ENV.md describes both hooks.

First clone? Just run `./koru.sh run` — it fetches the fonts for you.
EOF
}

# There is no gateway verb. The Koru API Gateway is a worker role of
# koru-agent -- `koru-agent --worker gateway` -- which reads a launch bootstrap
# from its supervisor's stdin and refuses to serve without one, so it cannot be
# started from a command line. Its `status`, `stop` and `unfiled` commands were
# deleted with the standalone executable and became no Agent command.
#
# There is no replacement for the foreground run: it is the one capability that
# went with the standalone executable rather than moving into the Agent. `./koru.sh run-all` starts an Agent with
# --console and a gateway under it, which is the nearest thing; a supervised
# gateway writes only to <store root>/logs/koru-api-gateway.log.<date>, never to
# a console, because `init_logging` gives it the file sink alone so a child and
# its parent cannot contend for one destination. Tail the file.
#
# For the other two verbs: `koru-agent status` prints a `gateway:` line, probed
# against the gateway's own rendezvous; and stopping the Agent stops its workers
# with it, so there is nothing left to ask by name.

# Run or inspect the voice worker. Started on demand by Agent the first time
# somebody dictates, so a launch from here is a diagnostic. It takes the store
# root, the way the gateway does, because that is where its endpoint, its logs
# and its model cache are resolved from.
voice_verb() {
  local -a root_args=()
  if [ -n "${KORU_ROOT:-}" ]; then
    root_args=(--root "$KORU_ROOT/store")
  fi
  exec cargo run -p koru-voice-worker --bin koru-voice-worker -- "${root_args[@]}" "$@"
}

# The whole debug stack as one supervised set — the Unix half of
# `koru.bat run-all` / `scripts/dev/run-all.ps1`, with the same shape: Agent first,
# its supervised gateway second, then the requested UI processes. Services stay
# up while either Koru or Koru AI Chat is open; Ctrl+C stops the complete set.
# KORU_ROOT names an explicit channel root, exactly as on Windows.
#
# Agent is launched with SIGINT *ignored* — the disposition survives
# exec — so a Ctrl+C in this terminal reaches only Koru and this script, and
# the script gets to stop the services politely instead of the terminal
# spraying INT across the whole process group.
RUN_ALL_SIGNALLED=0

# Quote a full executable path for `pgrep -f` / `pkill -f`. Those tools take a
# regular expression rather than a literal, and worktree names routinely carry
# dots, brackets or other metacharacters. Matching from argv[0] through its
# following whitespace makes this an exact image-path check, not a process-name
# sweep that could reach a sibling worktree or an installed Koru.
image_pattern() {
  printf '%s' "$1" | sed 's/[][\\.^$*+?(){}|]/\\&/g'
}

image_running() {
  local pattern
  pattern="$(image_pattern "$1")"
  pgrep -f "^${pattern}([[:space:]]|$)" >/dev/null 2>&1
}

stop_exact_image() {
  local pattern
  pattern="$(image_pattern "$1")"
  pkill -f "^${pattern}([[:space:]]|$)" 2>/dev/null || true
}

worktree_ui_running() {
  local bin="$1"
  image_running "$bin/koru" || image_running "$bin/koru-ai-chat"
}

worktree_stack_running() {
  local bin="$1" name
  # The retired standalone images are still swept: an older build of this
  # worktree may have left one running, and it holds the same root.
  for name in koru koru-ai-chat koru-agent koru-session-host koru-api-gateway koru-mcp koru-voice-worker koru-store-server; do
    image_running "$bin/$name" && return 0
  done
  return 1
}

run_all_signal() {
  RUN_ALL_SIGNALLED=1
}

run_all_stop() {
  local bin="$PWD/target/debug"
  echo
  echo "[Koru] stopping this worktree's stack..."
  # **The Agent takes its workers with it.** The gateway and the session host
  # are roles of koru-agent, ended by their supervisor, so there is nothing left
  # to ask by name.
  "$bin/koru-agent" ${AGENT_ROOT_ARGS[@]+"${AGENT_ROOT_ARGS[@]}"} stop >/dev/null 2>&1 || true
  "$bin/koru-ai-chat" ${CHAT_ROOT_ARGS[@]+"${CHAT_ROOT_ARGS[@]}"} stop >/dev/null 2>&1 || true
  # The voice worker is started by Agent on demand, so it is asked after it:
  # nothing is left to put one back.
  "$bin/koru-voice-worker" ${STORE_ROOT_ARGS[@]+"${STORE_ROOT_ARGS[@]}"} stop >/dev/null 2>&1 || true
  # Politeness has a deadline. The fallback matches full executable paths, so
  # it also catches a chat opened later from the caption without reaching a
  # sibling worktree or installed Koru.
  local deadline=$((SECONDS + 5))
  while worktree_stack_running "$bin" && [ "$SECONDS" -lt "$deadline" ]; do
    sleep 0.1 || true
  done
  local name
  for name in koru koru-ai-chat koru-agent koru-session-host koru-api-gateway koru-mcp koru-voice-worker koru-store-server; do
    stop_exact_image "$bin/$name"
  done
}

run_all_verb() {
  # Agent discovers its siblings beside its own executable, so everything
  # builds into the same debug directory.
  # koru-agent hosts the gateway and the session host as roles of its own
  # image, so building it builds them.
  local -a packages=(-p koru-agent -p koru -p koru-ai-chat)
  builds_voice_worker && packages+=(-p koru-voice-worker)
  local mcp=0 ai=0
  for arg in "$@"; do
    case "$arg" in
      --mcp) mcp=1; packages+=(-p koru-mcp) ;;
      --ai) ai=1 ;;
      *)
        echo "koru: $sub accepts only --mcp and --ai" >&2
        echo "usage: ./koru.sh $sub [--mcp] [--ai]" >&2
        exit 2 ;;
    esac
  done
  # The same file as `run`, for koru.bat's reason: the stack this brings up
  # signs in and syncs, so it must be as impossible to point at the wrong Cloud
  # without noticing. Without it this would run with whatever KORU_* the shell
  # held — usually nothing, so no Cloud at all.
  load_env_file .env.production
  ensure_fonts
  ensure_voice_runtime
  cargo build "${packages[@]}" || exit 1

  local bin="$PWD/target/debug"
  local -a agent_start=(--console --tray) app_args=()
  STORE_ROOT_ARGS=() AGENT_ROOT_ARGS=() CHAT_ROOT_ARGS=()
  RUN_ALL_SIGNALLED=0
  if [ -n "${KORU_ROOT:-}" ]; then
    local root
    root="$(cd "$(dirname "$KORU_ROOT")" && pwd)/$(basename "$KORU_ROOT")"
    echo "[Koru] channel root: $root"
    # The same layout run-all.ps1 uses: an app-level root is a *channel* root,
    # the store's directory one level inside it.
    STORE_ROOT_ARGS=(--root "$root")
    AGENT_ROOT_ARGS=(--root "$root")
    CHAT_ROOT_ARGS=(--root "$root")
    agent_start=(--root "$root" --console --tray)
    app_args=(--root "$root")
  fi

  trap run_all_signal INT TERM
  ( trap '' INT; exec "$bin/koru-agent" "${agent_start[@]}" ) &
  local agent_pid=$!
  local ready=0
  local attempt agent_status
  for ((attempt = 0; attempt < 100; attempt++)); do
    sleep 0.1 || true
    [ "$RUN_ALL_SIGNALLED" = 1 ] && break
    kill -0 "$agent_pid" 2>/dev/null || break
    agent_status="$("$bin/koru-agent" ${AGENT_ROOT_ARGS[@]+"${AGENT_ROOT_ARGS[@]}"} status 2>/dev/null || true)"
    case "$agent_status" in
      "running, version "*) ready=1; break ;;
    esac
  done
  if [ "$RUN_ALL_SIGNALLED" = 1 ]; then
    run_all_stop
    trap - INT TERM
    return 0
  fi
  if [ "$ready" != 1 ]; then
    run_all_stop
    echo "koru: the Koru Agent did not become ready" >&2
    exit 1
  fi

  # **Asked of the Agent, and probed rather than believed.** `koru-agent status`
  # prints a `gateway:` line it gets by dialling the gateway's own rendezvous,
  # so a wedged supervisor cannot report a stack that is not there. It replaced
  # `koru-api-gateway status`, which went with the executable.
  ready=0
  for ((attempt = 0; attempt < 200; attempt++)); do
    sleep 0.1 || true
    [ "$RUN_ALL_SIGNALLED" = 1 ] && break
    kill -0 "$agent_pid" 2>/dev/null || break
    agent_status="$("$bin/koru-agent" ${AGENT_ROOT_ARGS[@]+"${AGENT_ROOT_ARGS[@]}"} status 2>/dev/null || true)"
    case "$agent_status" in
      *"gateway:   serving"*) ready=1; break ;;
    esac
  done
  if [ "$RUN_ALL_SIGNALLED" = 1 ]; then
    run_all_stop
    trap - INT TERM
    return 0
  fi
  if [ "$ready" != 1 ]; then
    run_all_stop
    echo "koru: the Koru API Gateway did not become ready" >&2
    exit 1
  fi

  if [ "$mcp" = 1 ]; then
    if ! "$bin/koru-mcp" doctor ${AGENT_ROOT_ARGS[@]+"${AGENT_ROOT_ARGS[@]}"}; then
      run_all_stop
      if [ "$RUN_ALL_SIGNALLED" = 1 ]; then
        trap - INT TERM
        return 0
      fi
      echo "koru: the MCP bridge could not reach the API Gateway" >&2
      exit 1
    fi
    echo "[Koru] MCP bridge ready for a stdio host to launch"
  fi

  if [ "$RUN_ALL_SIGNALLED" = 1 ]; then
    run_all_stop
    trap - INT TERM
    return 0
  fi

  if [ "$ai" = 1 ]; then
    "$bin/koru-ai-chat" ${CHAT_ROOT_ARGS[@]+"${CHAT_ROOT_ARGS[@]}"} &
    local ai_pid=$!
    echo "[Koru] Koru AI Chat started, pid $ai_pid"
  fi

  echo "[Koru] API Gateway ready, Agent pid $agent_pid"
  echo "[Koru] launching Koru; Ctrl+C stops this worktree's complete stack"
  "$bin/koru" ${app_args[@]+"${app_args[@]}"} &
  local app_pid=$!

  # Fork happens before exec replaces the child with the exact image. Give
  # that handoff a bounded moment so the first exact-path probe cannot mistake
  # a healthy launch for an already-closed UI.
  for ((attempt = 0; attempt < 100; attempt++)); do
    [ "$RUN_ALL_SIGNALLED" = 1 ] && break
    image_running "$bin/koru" && break
    sleep 0.01 || true
  done

  # A chat launched later by the workspace caption is not our child, so PIDs
  # alone are insufficient. Keep Agent and Gateway available until no Koru UI
  # from this exact target/debug directory remains.
  while worktree_ui_running "$bin"; do
    [ "$RUN_ALL_SIGNALLED" = 1 ] && break
    sleep 0.1 || true
  done
  run_all_stop
  trap - INT TERM

  local app_exit=0 ai_exit=0
  wait "$app_pid" 2>/dev/null || app_exit=$?
  if [ "$ai" = 1 ]; then
    wait "$ai_pid" 2>/dev/null || ai_exit=$?
  fi
  # A signal is the ordinary way this runner ends; cleanup deliberately hides
  # the UI exit codes it caused. Without one, preserve unexpected failures.
  if [ "$RUN_ALL_SIGNALLED" != 1 ] && [ "$app_exit" != 0 ]; then
    echo "koru: Koru exited with code $app_exit" >&2
    exit "$app_exit"
  fi
  if [ "$RUN_ALL_SIGNALLED" != 1 ] && [ "$ai_exit" != 0 ]; then
    echo "koru: Koru AI Chat exited with code $ai_exit" >&2
    exit "$ai_exit"
  fi
}

# The escape hatch for a stack something else leaked (a test run, a crashed
# run-all): stop this worktree's exact-path binaries and nothing else — the
# Unix half of `koru.bat stop-all`.
stop_all_verb() {
  local bin="$PWD/target/debug"
  if [ "$#" -ne 0 ]; then
    echo "koru: stop-all takes no arguments" >&2
    echo "usage: ./koru.sh stop-all" >&2
    exit 2
  fi
  "$bin/koru-agent" stop >/dev/null 2>&1 || true
  "$bin/koru-ai-chat" stop >/dev/null 2>&1 || true
  "$bin/koru-voice-worker" stop >/dev/null 2>&1 || true
  sleep 1
  for name in koru koru-ai-chat koru-agent koru-session-host koru-api-gateway koru-mcp koru-voice-worker koru-store-server; do
    stop_exact_image "$bin/$name"
  done
  echo "[Koru] this worktree's Agent and its gateway and sessions workers, MCP, voice worker, diagnostic Store host, Koru and Koru AI Chat are stopped."
}

# A stdio MCP server belongs to the client that starts it, so a detached launch
# has no useful peer. The plain target is a discovery check; --console is the
# explicit foreground transport for protocol inspection and manual clients.
mcp_verb() {
  local -a command
  if [ "${1:-}" = "--console" ]; then
    shift
    command=(serve --stdio "$@")
  elif [ "$#" -eq 0 ]; then
    command=(doctor)
  else
    command=("$@")
  fi
  if [ -n "${KORU_ROOT:-}" ]; then
    command+=(--root "$KORU_ROOT")
  fi
  exec cargo run -p koru-mcp -- "${command[@]}"
}

# Which Cloud a launch talks to is a file, not a flag you can forget —
# koru.bat's rule, for koru.bat's reason: a desktop pointed at the wrong API
# signs in, enables Cloud sync and uploads somebody's pictures to it, and
# nothing on screen says which one it reached. So the Cloud's settings come
# from one gitignored file, `.env.production`; reference/ENV.md lists what goes
# in it. An empty file is the explicit choice of no Cloud.
#
# **Anything already set wins** — a value typed in the shell or chosen through
# `setup-envs` is a deliberate one-off — but a shell value that disagrees with
# the file is announced, because that is how somebody ends up on production
# with a localhost support URL.
#
# **No file is no Cloud, said in one line.** A checkout without a Cloud
# configuration — every community checkout — starts with Koru Cloud not
# configured, and the app says "Koru Cloud isn't set up in this build" wherever
# sign-in would be. The Cloud's own names are unset, so a stale value left in
# this shell cannot quietly become the Cloud a launch with no file talks to;
# that is the wrong-Cloud mistake the file switch exists to prevent. With a
# file, everything below applies unchanged.
load_env_file() {
  local file="$1" line key value
  if [ ! -f "$file" ]; then
    unset KORU_PUBLIC_API_ORIGIN KORU_PUBLIC_API_ISSUER KORU_PUBLIC_API_CREDENTIAL_ROLLOUT
    echo "[Koru] no $file: starting with Koru Cloud not configured; CONTRIBUTING.md says how to set one up."
    return 0
  fi
  echo "[Koru] configuration: $file"
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line%$'\r'}"
    case "$line" in ''|'#'*) continue ;; esac
    case "$line" in *=*) ;; *) continue ;; esac
    # Everything after the first `=` is the value, untouched: one of these is
    # a JSON policy full of quotes and colons.
    key="${line%%=*}"
    value="${line#*=}"
    [[ "$key" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
    if [ -z "${!key+set}" ]; then
      export "$key=$value"
    elif [ "${!key}" != "$value" ]; then
      echo "[Koru] WARNING: this shell overrides $file: $key=${!key}" >&2
      echo "[Koru]          the file says $key=$value; unset it or open a new terminal to use it." >&2
    fi
  done < "$file"
}

sub="${1:-run}"
# `--window-size` is an app flag, not a verb. Leave it in "$@" so
# `./koru.sh --window-size 1024x768` is the same launch as
# `./koru.sh run -- --window-size 1024x768`.
case "$sub" in
  --window-size|--window-size=*) sub=run ;;
  *) [ $# -gt 0 ] && shift ;;
esac

# **sccache is required to build, so say so here rather than in cargo's voice.**
# `.cargo/config.toml` sets `rustc-wrapper = "sccache"` and there is no optional
# form of that: without it on PATH every cargo command fails with an error about
# a missing program, which reads as a broken checkout rather than a missing tool.
# `setup` is the verb that tells you sccache is missing, so it is not gated.
case "$sub" in setup) needs_sccache=0 ;; *) needs_sccache=1 ;; esac
if [ "$needs_sccache" = 1 ] && ! command -v sccache >/dev/null 2>&1; then
  echo "koru: sccache is not on PATH, and this repository builds through it." >&2
  echo >&2
  echo "  brew install sccache   # macOS;  winget install Mozilla.sccache  or: cargo install sccache" >&2
  echo >&2
  echo "Every worktree shares one compiler cache, which is what makes a second" >&2
  echo "worktree cheap to build. See .cargo/config.toml, and ./koru.sh setup." >&2
  exit 1
fi

# **Three crates are built from patched source that is fetched, not committed**
# (third-party/README.md), and the workspace names them by path, so every verb
# that reaches cargo needs them first. The script returns at once when its
# stamp matches the pins. `help` does not need them; nothing else is worth a
# special case.
case "$sub" in
  # `setup` does these itself, and `setup --check` must change nothing.
  help|-h|--help|setup) ;;
  *)
    bash scripts/setup/fetch-third-party.sh || exit 1
    ;;
esac

case "$sub" in
  setup)    setup_verb "$@" ;;
  run)
    ensure_fonts
    load_env_file .env.production
    if needs_app_separator "$@"; then
      exec cargo run -- "$@"
    else
      exec cargo run "$@"
    fi
    ;;
  run-all)  run_all_verb "$@" ;;
  stop-all) stop_all_verb "$@" ;;
  # The table comes from the binary, not from this script: `koru --envs`
  # prints crates/koru-env's registry, the same one reference/ENV.md is generated
  # from. The built exe answers instantly; cargo only runs when nothing is
  # built yet.
  envs)
    if [ -x target/debug/koru ]; then exec target/debug/koru --envs
    elif [ -x target/release/koru ]; then exec target/release/koru --envs
    else exec cargo run -q -p koru-env
    fi
    ;;
  # A child cannot set its parent shell's environment, so the walk happens on
  # stderr and the chosen `export`/`unset` lines go to stdout — apply them
  # with:  eval "$(./koru.sh setup-envs)"
  setup-envs)
    cargo build -q -p koru-env
    cargo run -q -p koru-env -- --tsv | while IFS="$(printf '	')" read -r name kind purpose; do
      [ "$kind" = "switch" ] || continue
      printf '%s\n  %s\n  on, off, or skip? [y/n/s] ' "$name" "$purpose" >&2
      read -r answer </dev/tty
      case "$answer" in
        y|Y) printf 'export %s=1\n' "$name" ;;
        n|N) printf 'unset %s\n' "$name" ;;
        *)   ;;
      esac
      echo >&2
    done
    echo "[Koru] apply the lines above with:  eval \"\$(./koru.sh setup-envs)\"" >&2
    ;;
  release)
    ensure_fonts
    if needs_app_separator "$@"; then
      exec cargo run --release -- "$@"
    else
      exec cargo run --release "$@"
    fi
    ;;
  # Same launch, with the stall watch on and writing where it can be attached to
  # a report. A verb rather than "set these two variables" because the moment
  # somebody needs this is the moment they are annoyed, and the log is only
  # useful if it was already running when the freeze happened.
  jank)
    ensure_fonts
    export KORU_JANK=1
    export KORU_JANK_LOG="${KORU_JANK_LOG:-$PWD/koru-jank.log}"
    echo "[Koru] stalls over ${KORU_JANK_MS:-50}ms -> $KORU_JANK_LOG"
    if needs_app_separator "$@"; then
      exec cargo run -- "$@"
    else
      exec cargo run "$@"
    fi
    ;;
  voice)    voice_verb "$@" ;;
  mcp)      mcp_verb "$@" ;;
  document) ensure_fonts; exec cargo run -p koru-document "$@" ;;
  # The whole package — every shipped binary and all thirty fonts — staged and
  # zipped into dist/cloud/, and then nothing: no signing, no upload. It replaced
  # an older dist script, which built koru.exe alone into a nested layout the updater
  # refuses, and which now dies at launch because koru.exe needs its gateway.
  dist-local) ensure_fonts; exec cargo run --release -p koru-release -- dist-local "$@" ;;
  # The same package, published: tagged, signed and uploaded from a clean
  # tree, with release credentials this script never reads.
  dist-cloud) ensure_fonts; exec cargo run --release -p koru-release -- dist-cloud "$@" ;;
  # The Safari posture without Safari: unzip the newest dist-local archive and
  # stamp the quarantine xattr a browser download carries, so the Gatekeeper
  # story is testable before anything reaches the cloud. Double-click
  # dist/try/Koru.app for the dialog a downloader sees; strip the xattr
  # (xattr -dr com.apple.quarantine dist/try/Koru.app) for the working flow.
  dist-try)
    [ "$(uname)" = Darwin ] || { echo "[Koru] dist-try is a macOS verb" >&2; exit 2; }
    artifact=$(ls -t dist/cloud/koru-*-macos-*.dmg dist/cloud/koru-*-macos-*.zip 2>/dev/null | head -1)
    [ -n "$artifact" ] || { echo "[Koru] no mac artifact in dist/cloud; run ./koru.sh dist-local first" >&2; exit 2; }
    rm -rf dist/try && mkdir -p dist/try
    case "$artifact" in
      *.dmg)
        # The download path a person walks: mount the image, drag the app
        # out. ditto stands in for the drag; the quarantine stamp is what
        # Finder would have propagated from the browser's download.
        mount=$(mktemp -d)
        hdiutil attach -nobrowse -readonly -mountpoint "$mount" "$artifact" >/dev/null
        ditto "$mount/Koru.app" dist/try/Koru.app
        hdiutil detach "$mount" >/dev/null
        ;;
      *) unzip -q "$artifact" -d dist/try ;;
    esac
    xattr -w com.apple.quarantine "0083;$(printf %x "$(date +%s)");Safari;" dist/try/Koru.app
    echo "[Koru] $artifact -> dist/try/Koru.app (quarantined, as a browser download)"
    # gktool exits nonzero on the verdict this exists to show, and writes
    # carriage-return progress; both would otherwise eat the instructions.
    gktool scan dist/try/Koru.app 2>&1 | tr '\r' '\n' | grep -v '^Progress:' \
      | sed 's/^/[Koru] gatekeeper: /' || true
    echo "[Koru] double-click dist/try/Koru.app for the downloader's dialog;"
    echo "[Koru] xattr -dr com.apple.quarantine dist/try/Koru.app  # then it opens"
    exit 0 ;;
  # The GitHub nightly's notes, drafted from the commits since the `nightly`
  # tag for the maintainer to edit and commit. It builds nothing, so no fonts;
  # CI's `nightly-notes --select` goes through release-tool.
  nightly-notes) exec cargo run --release -p koru-release -- nightly-notes "$@" ;;
  # Any koru-release subcommand. A dist-* one packages, so it gets the fonts
  # dist-local gets; the others build nothing — they read or write what the
  # release origin serves. The subcommands that publish need release
  # credentials this script never reads.
  release-tool)
    case "${1:-}" in
      "") echo "koru: release-tool needs a koru-release subcommand" >&2
          echo "usage: ./koru.sh release-tool <subcommand> [args...]" >&2
          exit 2 ;;
      dist-*) ensure_fonts ;;
    esac
    exec cargo run --release -p koru-release -- "$@" ;;
  # No ensure_fonts: it fetches somebody else's weights, repackages them and
  # publishes them. It writes to production only behind --upload.
  voice-models) exec cargo run --release -p koru-release -- voice-models "$@" ;;
  voice-test) exec bash scripts/dev/voice-test.sh "$@" ;;
  # Through the binary rather than reimplemented here, so it resolves the
  # profile folder with the same code a normal launch does. A shell script that
  # guessed would eventually back up the wrong folder, quietly.
  backup)   cargo run --quiet -p koru -- --backup "$@" ;;
  build)    ensure_voice_runtime; exec cargo build "$@" ;;
  # **Both halves are capped in `.cargo/config.toml`, and neither is a flag
  # here.** `[build] jobs` caps rustc; the harness that runs the compiled tests
  # has its own pool and does not read that key, so it is capped separately by
  # `[env] RUST_TEST_THREADS`, at eight. This script passes neither — a number
  # written at a call site is one two launchers have to keep in step and a
  # hand-typed `cargo test` never gets. An explicit `-- --test-threads=N` still
  # wins over the env var, so `./koru.sh test -- --test-threads=1` serialises a
  # suite you are debugging.
  #
  # No pre-emptive stop here, unlike `koru.bat test`. That exists because
  # Windows locks a running image and the suite may leave an Agent and gateway
  # behind, so the next build cannot replace them. Unix replaces a running
  # binary happily, so there is nothing to clear before a build; `stop-all` is
  # still here for a stack a test run leaves behind.
  test)     ensure_voice_runtime; exec cargo test --workspace ${WORKSPACE_EXCLUDES[@]+"${WORKSPACE_EXCLUDES[@]}"} "$@" ;;
  fmt)      exec cargo fmt --all "$@" ;;
  check)    ensure_voice_runtime; exec cargo check --workspace ${WORKSPACE_EXCLUDES[@]+"${WORKSPACE_EXCLUDES[@]}"} "$@" ;;
  fonts)    exec bash scripts/setup/fetch-fonts.sh ;;
  # Swap the logo: regenerate the .ico and rebuild, so the embedded mark
  # (crates/koru-draw/src/brand.rs) and window icon (koru-workspace-host/src/app.rs) pick up
  # the new logo.png. The taskbar pin refreshes itself on launch on Windows
  # (apps/koru/src/taskbar.rs), so no extra step there.
  logo)     python3 scripts/icons/set-logo.py && cargo build -p koru ;;
  clean)    exec cargo clean "$@" ;;
  help|-h|--help) usage ;;
  *)
    echo "koru: unknown command '$sub'" >&2
    echo >&2
    usage >&2
    exit 2
    ;;
esac
