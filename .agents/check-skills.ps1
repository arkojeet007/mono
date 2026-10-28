# SPDX-License-Identifier: MPL-2.0

# The Windows twin of .agents/check-skills.sh: are the installed agent skills
# (.agents\skills, from koruworks/skills) behind what koruworks/skills has
# published? Prints one line when they are, and nothing when they are current,
# not installed, or the network is away. It never updates on its own - the
# line it prints asks the agent to ask the user. .agents\check-skills.bat runs
# it, for cmd and the launchers; from PowerShell, call it directly.
#
#   .agents\check-skills.bat            report, from a remote answer cached for a day
#   .agents\check-skills.bat --now      ask koruworks/skills again first
#   .agents\check-skills.bat --update   install the latest and record it
#   .agents\check-skills.bat --record   record the published commit as the
#                                       installed one (the launchers call this)
#
# The record is .agents\skills.source, gitignored, two lines shared with the
# shell twin: "installed <commit>" and "checked <epoch> <commit>". Keep the
# modes, the messages and the record in step with check-skills.sh.
#
# ASCII ONLY: Windows PowerShell 5.1 reads a BOM-less script as ANSI.
param([string]$Mode = '')
# `powershell -File x.ps1 --now` leaves a dash-dash argument in $args, not
# in $Mode, so take it from there; `-Mode --now` binds as usual.
if (-not $Mode -and $args.Count -gt 0) { $Mode = [string]$args[0] }

$Source = 'koruworks/skills'
$Remote = "https://github.com/$Source"
$Cli = 'skills@1.7.2'
$Day = 86400

$root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $root
$record = Join-Path $root '.agents\skills.source'

function Get-RemoteHead {
    # One round trip, no prompts, ten seconds at most: '' when git is missing,
    # the network is away, or the wait runs out.
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = 'git'
    $psi.Arguments = "ls-remote $Remote HEAD"
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables['GIT_TERMINAL_PROMPT'] = '0'
    try { $p = [System.Diagnostics.Process]::Start($psi) } catch { return '' }
    $out = $p.StandardOutput.ReadToEndAsync()
    $null = $p.StandardError.ReadToEndAsync()
    if (-not $p.WaitForExit(10000)) {
        try { $p.Kill() } catch { }
        return ''
    }
    if ($p.ExitCode -ne 0) { return '' }
    $head = ($out.Result -split "`t")[0].Trim()
    if ($head -match '^[0-9a-f]{40}$') { return $head }
    return ''
}

function Get-Now { return [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() }

function Short([string]$s) {
    if ($s.Length -gt 7) { return $s.Substring(0, 7) } else { return $s }
}

function Read-Record {
    $r = @{ installed = ''; checkedAt = [long]0; checkedHead = '' }
    if (Test-Path -LiteralPath $record) {
        foreach ($line in (Get-Content -LiteralPath $record)) {
            $f = $line.Trim() -split ' +'
            if ($f[0] -eq 'installed' -and $f.Count -ge 2) {
                $r.installed = $f[1]
            } elseif ($f[0] -eq 'checked' -and $f.Count -ge 3) {
                $at = [long]0
                if ([long]::TryParse($f[1], [ref]$at)) { $r.checkedAt = $at }
                $r.checkedHead = $f[2]
            }
        }
    }
    return $r
}

function Write-Record([string]$installed, [long]$at, [string]$head) {
    # LF and no BOM, so the shell twin reads the same file.
    $lines = @()
    if ($installed) { $lines += "installed $installed" }
    if ($head) { $lines += "checked $at $head" }
    $text = ($lines -join "`n") + "`n"
    [IO.File]::WriteAllText($record, $text, (New-Object System.Text.UTF8Encoding($false)))
}

function Record-Published {
    $head = Get-RemoteHead
    if (-not $head) {
        Write-Output "[skills] could not reach $Source to record which commit is installed; run: .agents\check-skills.bat --record"
        return
    }
    Write-Record $head (Get-Now) $head
    Write-Output "[skills] installed $Source at $(Short $head)"
}

switch ($Mode) {
    '--record' { Record-Published; exit 0 }
    '--update' {
        # The CLI again, as the installers do; the .claude\skills link stays as
        # it is. Needs Node.js for npx.
        if (-not (Get-Command npx -ErrorAction SilentlyContinue)) {
            Write-Output "[skills] npx was not found; install Node.js, then run: .agents\check-skills.bat --update"
            exit 1
        }
        Write-Output "[skills] installing the agent skills from $Source"
        & npx -y $Cli add $Source --agent codex --yes | Out-Null
        if ($LASTEXITCODE -ne 0) {
            Write-Output "[skills] the update failed; run by hand: npx -y $Cli add $Source --agent codex --yes"
            exit 1
        }
        Record-Published
        exit 0
    }
    '--now' { }
    '' { }
    default {
        Write-Output "usage: .agents\check-skills.bat [--now | --update | --record]"
        exit 2
    }
}

# Nothing installed is the installer's business, not a staleness report.
if (-not (Get-ChildItem -Path (Join-Path $root '.agents\skills\*\SKILL.md') -ErrorAction SilentlyContinue)) { exit 0 }

$r = Read-Record
if (-not $r.installed) {
    Write-Output "[skills] there is no record of which $Source commit is installed in .agents\skills; run: .agents\check-skills.bat --record (or --update for the latest)"
    exit 0
}

$now = Get-Now
$head = $r.checkedHead
if ($Mode -eq '--now' -or -not $head -or ($now - $r.checkedAt) -gt $Day) {
    $fresh = Get-RemoteHead
    if (-not $fresh) { exit 0 }   # offline: say nothing rather than guess
    $head = $fresh
    Write-Record $r.installed $now $head
}

if ($head -ne $r.installed) {
    Write-Output "[skills] the agent skills in .agents\skills are behind $Source (installed $(Short $r.installed), published $(Short $head)). Ask the user before updating; on a yes, run: .agents\check-skills.bat --update"
}
exit 0
