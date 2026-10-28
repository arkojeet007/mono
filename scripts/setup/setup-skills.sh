#!/bin/sh
# SPDX-License-Identifier: MPL-2.0

# Install Koru's agent skills from koruworks/skills with the skills CLI
# (https://skills.sh) into .agents/skills/, which Codex reads, and link
# .claude/skills to it for Claude Code, as one link for the whole directory.
# Neither is committed; skills-lock.json, which the CLI writes, is not either.
# After an install this records the published commit in .agents/skills.source,
# which .agents/check-skills.sh compares with koruworks/skills at session start.
#
#   sh scripts/setup/setup-skills.sh            install or repair what is missing
#   sh scripts/setup/setup-skills.sh --update   install again, taking the latest
#
# **The link is a symbolic link, not a junction, where it can be.** On
# Windows `git worktree remove` cannot delete a junction (nor the per-skill
# junctions the CLI makes by default): it unregisters the worktree and leaves
# its directory behind. A directory symlink it deletes cleanly. Creating one
# needs Developer Mode (or an elevated shell); without it this falls back to a
# junction, which Claude Code reads just as well, and says so.
#
# Also points core.hooksPath at .githooks when nothing else has set it, so
# that `git worktree add` runs .githooks/post-checkout, which calls this.
# git never runs a repository's hooks on `git clone`, so a fresh clone gets
# both from the explicit `setup --agent-skills` option on either launcher.
#
# The skills CLI's install telemetry is left on: each install sends skills.sh
# an anonymous count of which skills went to which agents, which is how
# koruworks/skills is ranked there. DISABLE_TELEMETRY=1 turns it off.
#
# Keep the source, the agent, the CLI version and the link in step with
# :ensure_skills in koru.bat.
set -eu

SOURCE=koruworks/skills
CLI=skills@1.7.2

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

windows=0
changed=0
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) windows=1 ;; esac

if git rev-parse --is-inside-work-tree >/dev/null 2>&1 &&
   [ -z "$(git config --get core.hooksPath || true)" ]; then
  git config core.hooksPath .githooks
  echo "[skills] core.hooksPath is now .githooks, so a new worktree installs the agent skills"
fi

if [ "${1:-}" = "--update" ] || ! ls .agents/skills/*/SKILL.md >/dev/null 2>&1; then
  if ! command -v npx >/dev/null 2>&1; then
    echo "[skills] the agent skills are not installed: npx was not found." >&2
    echo "[skills] install Node.js, then run: sh scripts/setup/setup-skills.sh" >&2
    exit 1
  fi
  echo "[skills] installing the agent skills from $SOURCE"
  log="$(mktemp)"
  if npx -y "$CLI" add "$SOURCE" --agent codex --yes >"$log" 2>&1; then
    rm -f "$log"
    changed=1
    sh .agents/check-skills.sh --record || true
  else
    echo "[skills] the install failed; the CLI said:" >&2
    tail -n 20 "$log" >&2
    rm -f "$log"
    exit 1
  fi
fi

# Remove whatever .claude/skills is when it is not the link: an older install's
# copies, a link that points nowhere. A link is removed as a link, never by
# recursing into it, which on Windows would delete the skills it points at.
unlink_claude() {
  if [ "$windows" = 1 ]; then
    cmd //c "rmdir .claude\\skills" >/dev/null 2>&1 || rm -f .claude/skills
  else
    rm -f .claude/skills
  fi
}
if [ -L .claude/skills ] && ls .claude/skills/*/SKILL.md >/dev/null 2>&1; then
  :
else
  if [ -L .claude/skills ]; then
    unlink_claude
  elif [ -d .claude/skills ]; then
    rm -rf .claude/skills
  fi
  mkdir -p .claude
  changed=1
  if [ "$windows" = 1 ]; then
    if cmd //c "mklink /D .claude\\skills ..\\.agents\\skills" >/dev/null 2>&1; then
      echo "[skills] .claude/skills links to .agents/skills"
    elif cmd //c "mklink /J .claude\\skills .agents\\skills" >/dev/null 2>&1; then
      echo "[skills] .claude/skills is a junction to .agents/skills: Windows would not make a symlink" >&2
      echo "[skills]   without Developer Mode, and git worktree remove leaves a junction's worktree behind" >&2
    else
      echo "[skills] could not link .claude/skills to .agents/skills" >&2
      exit 1
    fi
  else
    ln -s ../.agents/skills .claude/skills
    echo "[skills] .claude/skills links to .agents/skills"
  fi
fi

# Silent when there was nothing to do: the hook runs this on every checkout.
if [ "$changed" = 1 ]; then
  count="$(ls -d .agents/skills/*/ 2>/dev/null | wc -l | tr -d ' ')"
  echo "[skills] $count skills in .agents/skills"
fi
