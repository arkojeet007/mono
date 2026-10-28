#!/bin/sh
# SPDX-License-Identifier: MPL-2.0

# Are the installed agent skills (.agents/skills, from koruworks/skills) behind
# what koruworks/skills has published? Prints one line when they are, and
# nothing when they are current, not installed, or the network is away. Any
# agent can run it: Claude Code runs it at session start through
# .claude/settings.json, and AGENTS.md asks the others to. It never updates on
# its own — the line it prints asks the agent to ask the user.
#
#   sh .agents/check-skills.sh            report, from a remote answer cached for a day
#   sh .agents/check-skills.sh --now      ask koruworks/skills again first
#   sh .agents/check-skills.sh --update   install the latest and record it
#   sh .agents/check-skills.sh --record   record the published commit as the
#                                         installed one (the installers call this)
#
# The record is .agents/skills.source, gitignored, two lines:
#   installed <commit>          the published HEAD when the skills were installed
#   checked <epoch> <commit>    the last answer from the remote, and when
# The skills CLI's own skills-lock.json records no commit, which is why this
# file exists. The same file lives in every repository that installs the skills.
# .agents/check-skills.ps1 (run by check-skills.bat) is the Windows twin: keep
# the modes, the messages and the record in step.
set -u

SOURCE=koruworks/skills
REMOTE="https://github.com/$SOURCE"
CLI=skills@1.7.2

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
record=.agents/skills.source
day=86400

remote_head() {
  # One round trip, no prompts, and a bounded wait where `timeout` exists
  # (coreutils on Linux and Git for Windows; macOS has none and waits on git).
  t=""
  command -v timeout >/dev/null 2>&1 && t="timeout 10"
  GIT_TERMINAL_PROMPT=0 $t git ls-remote "$REMOTE" HEAD 2>/dev/null | cut -f1
}

short() { printf '%.7s' "$1"; }

read_field() { awk -v k="$1" '$1 == k { print $2 }' "$record" 2>/dev/null | head -n 1; }
read_checked_at() { awk '$1 == "checked" { print $2 }' "$record" 2>/dev/null | head -n 1; }
read_checked_head() { awk '$1 == "checked" { print $3 }' "$record" 2>/dev/null | head -n 1; }

write_record() { # installed, checked-at, checked-head
  mkdir -p .agents
  {
    [ -n "$1" ] && echo "installed $1"
    [ -n "$3" ] && echo "checked $2 $3"
  } >"$record"
}

installer() {
  # The repository's own installer repairs the .claude/skills link as well;
  # the CLI alone is the fallback.
  if [ -f scripts/setup/setup-skills.sh ]; then echo "sh scripts/setup/setup-skills.sh --update"
  elif [ -f scripts/setup-skills.sh ]; then echo "sh scripts/setup-skills.sh --update"
  else echo "npx -y $CLI update --project --yes"
  fi
}

case "${1:-}" in
  --record)
    head="$(remote_head)"
    if [ -z "$head" ]; then
      echo "[skills] could not reach $SOURCE to record which commit is installed; run: sh .agents/check-skills.sh --record" >&2
      exit 0
    fi
    write_record "$head" "$(date +%s)" "$head"
    echo "[skills] installed $SOURCE at $(short "$head")"
    exit 0
    ;;
  --update)
    cmd="$(installer)"
    echo "[skills] $cmd"
    if sh -c "$cmd"; then
      # The installers record on their own; the CLI fallback does not.
      case "$cmd" in npx*) exec sh "$0" --record ;; esac
      exit 0
    fi
    echo "[skills] the update failed; see above" >&2
    exit 1
    ;;
  --now | "") ;;
  *)
    echo "usage: sh .agents/check-skills.sh [--now | --update | --record]" >&2
    exit 2
    ;;
esac

# Nothing installed is the installer's business, not a staleness report.
ls .agents/skills/*/SKILL.md >/dev/null 2>&1 || exit 0

installed="$(read_field installed)"
if [ -z "$installed" ]; then
  echo "[skills] there is no record of which $SOURCE commit is installed in .agents/skills; run: sh .agents/check-skills.sh --record (or --update for the latest)"
  exit 0
fi

now="$(date +%s)"
checked_at="$(read_checked_at)"
head="$(read_checked_head)"
if [ "${1:-}" = "--now" ] || [ -z "$head" ] || [ -z "$checked_at" ] || [ $((now - checked_at)) -gt "$day" ]; then
  fresh="$(remote_head)"
  [ -z "$fresh" ] && exit 0   # offline: say nothing rather than guess
  head="$fresh"
  write_record "$installed" "$now" "$head"
fi

if [ "$head" != "$installed" ]; then
  echo "[skills] the agent skills in .agents/skills are behind $SOURCE (installed $(short "$installed"), published $(short "$head")). Ask the user before updating; on a yes, run: sh .agents/check-skills.sh --update"
fi
exit 0
