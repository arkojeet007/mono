# SPDX-License-Identifier: MPL-2.0

# Check a development machine against what this repository needs to build
# well, and make each change it can make safely; what it cannot, it lists with
# the command that does it. Run it again and it does only what is still not
# done, retrying whatever failed. With -Check it reports and changes nothing.
#
# ASCII ONLY. Windows PowerShell 5.1 reads a BOM-less script as ANSI, so a UTF-8
# dash or curly quote arrives as three cp1252 characters, one of which opens a
# string and breaks the parse pages later. Same rule as defender-exclusions.ps1.
#
# USAGE
#
#   .\koru.bat setup            set up what is not, report the rest
#   .\koru.bat setup --check    report only
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\setup\setup-check.ps1 [-Check]
#
# WHAT IT CHANGES, AND WHAT IT LEAVES ALONE
#
# It installs sccache with winget; sets SCCACHE_DIR (only when unset, on the
# fastest drive), SCCACHE_CACHE_SIZE and CARGO_BUILD_JOBS at the user scope;
# restarts the sccache server so it reads them; and fetches the fonts.
# With -DefenderExclusions it runs defender-exclusions.ps1, elevated:
# a marker in this checkout's git directory records that it ran, because an
# unelevated shell cannot read the list back. A SCCACHE_DIR you set yourself
# is never moved, and nothing is changed for a check that already passes.
# koru.bat fetches third-party code first. Agent skills and their hook are
# installed only with setup --agent-skills.
#
# WHAT IT CHECKS, AND WHY EACH ONE IS HERE
#
# Every one of these was found the slow way: a build that took the desktop
# with it, a test run killed for memory, a cache that evicted every night, a
# worktree whose font tests failed because nobody had fetched the fonts. Each
# check prints the exact command that fixes it. Run it on a new machine, and
# again whenever a build feels slower than it should.
#
#   sccache      on PATH - the repository builds through it, no optional form
#   SCCACHE_DIR  set, and on the fastest drive this machine has
#   cache size   raised from sccache's 10 GiB default, which this workspace
#                outgrows in a day across two worktrees
#   Defender     excluding the repository, the target dirs and the cache; only
#                an elevated shell can read the list, so unelevated it says so
#   cargo jobs   a count that fits this machine's memory - rustc is memory
#                bound here, and a link phase with too many jobs has killed a
#                full-suite run on a 31 GB machine
#   fonts        the bundled faces present in THIS worktree; they are
#                gitignored and fetched, so a fresh worktree has none
#
# Exit code is the number of WARN lines, so a script can gate on it.

[CmdletBinding()]
param([switch]$Check, [switch]$DefenderExclusions)
$Apply = -not $Check

$ErrorActionPreference = "Continue"
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$script:warnings = 0
$script:fixes = 0
$script:restartServer = $false

function Fixed([string]$what, [string]$detail) {
    $script:fixes++
    Write-Host ("  fixed  {0}" -f $what) -ForegroundColor Green
    if ($detail) { Write-Host ("         {0}" -f $detail) -ForegroundColor DarkGray }
}
function Set-UserSetting([string]$name, [string]$value) {
    [Environment]::SetEnvironmentVariable($name, $value, "User")
    Set-Item -Path "Env:$name" -Value $value
}

function Pass([string]$what, [string]$detail) {
    Write-Host ("  ok     {0}" -f $what) -ForegroundColor Green
    if ($detail) { Write-Host ("         {0}" -f $detail) -ForegroundColor DarkGray }
}
function Warn([string]$what, [string]$detail, [string[]]$fix) {
    $script:warnings++
    Write-Host ("  WARN   {0}" -f $what) -ForegroundColor Yellow
    if ($detail) { Write-Host ("         {0}" -f $detail) }
    foreach ($f in $fix) { Write-Host ("         > {0}" -f $f) -ForegroundColor Cyan }
}
function Unknown([string]$what, [string]$detail, [string[]]$fix) {
    Write-Host ("  ?      {0}" -f $what) -ForegroundColor Magenta
    if ($detail) { Write-Host ("         {0}" -f $detail) }
    foreach ($f in $fix) { Write-Host ("         > {0}" -f $f) -ForegroundColor Cyan }
}
function Note([string]$what) { Write-Host ("         {0}" -f $what) -ForegroundColor DarkGray }

# "50G", "100GiB", "2T", "512M" -> GiB. sccache accepts the same spellings.
function Parse-Gib([string]$s) {
    if ($s -match '^\s*(\d+)\s*([KMGT]?)(i?B)?\s*$') {
        $n = [double]$matches[1]
        switch ($matches[2].ToUpper()) {
            "T" { return $n * 1024 }
            "G" { return $n }
            "M" { return $n / 1024 }
            "K" { return $n / 1048576 }
            default { return $n / 1073741824 }
        }
    }
    return $null
}

Write-Host ""
Write-Host ("Koru setup check for {0}" -f $repo) -ForegroundColor Cyan
Write-Host ""

# --------------------------------------------------------------- sccache
$sccache = Get-Command sccache -ErrorAction SilentlyContinue
if ($sccache) {
    Pass "sccache is on PATH" $sccache.Source
} elseif ($Apply -and (Get-Command winget -ErrorAction SilentlyContinue)) {
    & winget install --id Mozilla.sccache --exact --silent --accept-source-agreements --accept-package-agreements
    if ($LASTEXITCODE -eq 0) {
        Fixed "sccache installed with winget" "Open a new terminal: this one's PATH predates it."
    } else {
        Warn "sccache is not on PATH, and winget could not install it" $null @("winget install Mozilla.sccache")
    }
} else {
    Warn "sccache is not on PATH" `
        "Every cargo command in this repository runs through it; there is no optional form." `
        @("winget install Mozilla.sccache")
}

# --------------------------------------------------------------- drives
# Letter -> bus/media, read without elevation. NVMe beats SATA SSD beats HDD.
$kinds = @{}
try {
    $physical = Get-PhysicalDisk
    foreach ($p in (Get-Partition | Where-Object DriveLetter)) {
        $pd = $physical | Where-Object { $_.DeviceId -eq [string]$p.DiskNumber }
        if ($pd) {
            $kinds[[string]$p.DriveLetter] = @{
                Media = [string]$pd.MediaType; Bus = [string]$pd.BusType; Name = [string]$pd.FriendlyName
            }
        }
    }
} catch { }
function Drive-Rank($k) {
    if (-not $k) { return 0 }
    if ($k.Bus -eq "NVMe") { return 3 }
    if ($k.Media -eq "SSD") { return 2 }
    if ($k.Media -eq "HDD") { return 1 }
    return 0
}
function Describe-Drive([string]$letter) {
    $k = $kinds[$letter]
    if ($k) { return ("{0}: ({1} {2}, {3})" -f $letter, $k.Bus, $k.Media, $k.Name) }
    return ("{0}: (media unknown)" -f $letter)
}
$best = $null; $bestRank = 0
foreach ($letter in $kinds.Keys) {
    $r = Drive-Rank $kinds[$letter]
    if ($r -gt $bestRank) { $bestRank = $r; $best = $letter }
}

# A variable set at the user scope after this shell started is invisible to
# $env:, so read the persisted scopes as the fallback and say when the shell
# is the stale one - otherwise the check would tell a person to set what they
# just set.
function Get-Setting([string]$name) {
    $v = [Environment]::GetEnvironmentVariable($name, "Process")
    if ($v) { return @{ Value = $v; Stale = $false } }
    $v = [Environment]::GetEnvironmentVariable($name, "User")
    if (-not $v) { $v = [Environment]::GetEnvironmentVariable($name, "Machine") }
    if ($v) { return @{ Value = $v; Stale = $true } }
    return @{ Value = $null; Stale = $false }
}
$staleShell = "Set at the user scope, but this shell started before that; new terminals see it."

# --------------------------------------------------------------- SCCACHE_DIR
$default = Join-Path $env:LOCALAPPDATA "Mozilla\sccache\cache"
$dirSetting = Get-Setting "SCCACHE_DIR"
$env:SCCACHE_DIR = $dirSetting.Value
$cacheDir = if ($env:SCCACHE_DIR) { $env:SCCACHE_DIR } else { $default }
$cacheLetter = ([System.IO.Path]::GetPathRoot($cacheDir)).Substring(0, 1).ToUpper()
$cacheRank = Drive-Rank $kinds[$cacheLetter]
$suggest = if ($best) { $best } else { "D" }
$setDir = '[Environment]::SetEnvironmentVariable("SCCACHE_DIR", "{0}:\sccache\cache", "User")' -f $suggest
if (-not $env:SCCACHE_DIR -and $Apply) {
    $chosen = "{0}:\sccache\cache" -f $suggest
    New-Item -ItemType Directory -Force -Path $chosen | Out-Null
    Set-UserSetting "SCCACHE_DIR" $chosen
    $cacheDir = $chosen
    $cacheLetter = $suggest
    $script:restartServer = $true
    Fixed ("SCCACHE_DIR is now {0}, on {1}" -f $chosen, (Describe-Drive $suggest)) `
        "At the user scope; the cache at sccache's default is left where it is."
} elseif (-not $env:SCCACHE_DIR) {
    Warn "SCCACHE_DIR is not set, so the cache sits at sccache's default" `
        ("{0}, on {1}" -f $default, (Describe-Drive $cacheLetter)) `
        @($setDir, "sccache --stop-server   # the next cargo command starts one that reads it")
} elseif ($best -and $cacheRank -lt $bestRank) {
    # Never moved by setup: it was set by somebody, and a move starts cold.
    Warn ("SCCACHE_DIR is on {0}, not the fastest drive here" -f (Describe-Drive $cacheLetter)) `
        ("Fastest: {0}. Every cache hit is a file read; put it where reads are cheapest. setup leaves a SCCACHE_DIR you set alone." -f (Describe-Drive $best)) `
        @($setDir, "sccache --stop-server   # then move or discard the old cache directory")
} else {
    Pass ("SCCACHE_DIR is on {0}" -f (Describe-Drive $cacheLetter)) $cacheDir
}
if ($dirSetting.Stale) { Note $staleShell }
$persistedDir = [Environment]::GetEnvironmentVariable("SCCACHE_DIR", "User")
if (-not $persistedDir) { $persistedDir = [Environment]::GetEnvironmentVariable("SCCACHE_DIR", "Machine") }
if ($env:SCCACHE_DIR -and -not $persistedDir) {
    Note "Set in this shell only. Persist it at the user scope so every terminal and the server agree."
}

# --------------------------------------------------------------- cache size
$wantGib = 100
$sizeSetting = Get-Setting "SCCACHE_CACHE_SIZE"
$size = $sizeSetting.Value
$setSize = '[Environment]::SetEnvironmentVariable("SCCACHE_CACHE_SIZE", "{0}G", "User")' -f $wantGib
$restart = "sccache --stop-server   # the next cargo command starts one that reads it"
$sizeGib = if ($size) { Parse-Gib $size } else { $null }
if ($Apply -and (-not $size -or $sizeGib -eq $null -or $sizeGib -lt 50)) {
    $was = if ($size) { $size } else { "unset, so 10 GiB" }
    $size = "{0}G" -f $wantGib
    Set-UserSetting "SCCACHE_CACHE_SIZE" $size
    $sizeSetting = @{ Value = $size; Stale = $false }
    $script:restartServer = $true
    Fixed ("SCCACHE_CACHE_SIZE is now {0} (was {1})" -f $size, $was) "At the user scope."
}
if (-not $size) {
    Warn "SCCACHE_CACHE_SIZE is not set, so the ceiling is sccache's 10 GiB default" `
        "One workspace build across two worktrees is more than that; the cache evicts and recompiles what it just cached." `
        @($setSize, $restart)
} else {
    $gib = Parse-Gib $size
    if ($gib -eq $null) {
        Warn ("SCCACHE_CACHE_SIZE is '{0}', which sccache will not parse" -f $size) $null @($setSize, $restart)
    } elseif ($gib -lt 50) {
        Warn ("SCCACHE_CACHE_SIZE is {0}, below the 50 GiB this workspace needs to stop evicting" -f $size) `
            $null @($setSize, $restart)
    } else {
        Pass ("SCCACHE_CACHE_SIZE is {0}" -f $size)
    }
    if ($sizeSetting.Stale) { Note $staleShell }
    $persistedSize = [Environment]::GetEnvironmentVariable("SCCACHE_CACHE_SIZE", "User")
    if (-not $persistedSize) { $persistedSize = [Environment]::GetEnvironmentVariable("SCCACHE_CACHE_SIZE", "Machine") }
    if (-not $persistedSize) {
        Note "Set in this shell only. Persist it at the user scope so every terminal and the server agree."
    }
    # The ceiling must fit the drive, with room for the target directories
    # that share it.
    try {
        $freeGib = [math]::Round((Get-PSDrive -Name $cacheLetter).Free / 1GB)
        if ($gib -ne $null -and $freeGib -lt $gib) {
            Note ("{0}: has {1} GB free, less than the {2} GiB ceiling. sccache only stops growing when it reaches the ceiling." -f $cacheLetter, $freeGib, [int]$gib)
        }
    } catch { }
}
# A running server keeps the settings it started with. Compare its answer to
# the environment so a stale server is named rather than trusted.
if ($sccache) {
    $stats = & sccache --show-stats 2>$null
    $maxLine = ($stats | Select-String "Max cache size" | Select-Object -First 1)
    if ($maxLine) {
        $live = ($maxLine.Line -replace '^\s*Max cache size\s*', '').Trim()
        $liveGib = Parse-Gib ($live -replace '\s+', '')
        $envGib = if ($size) { Parse-Gib $size } else { 10 }
        if ($liveGib -ne $null -and $envGib -ne $null -and [math]::Abs($liveGib - $envGib) -ge 1 -and $Apply) {
            $script:restartServer = $true
        } elseif ($liveGib -ne $null -and $envGib -ne $null -and [math]::Abs($liveGib - $envGib) -ge 1) {
            Warn ("The running sccache server has a {0} ceiling; the environment says {1}" -f $live, $(if ($size) { $size } else { "10 GiB (default)" })) `
                "The server keeps the settings it started with." @("sccache --stop-server")
        } else {
            Note ("sccache server: max cache size {0}" -f $live)
        }
    }
}

# --------------------------------------------------------------- Defender
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$admin = (New-Object Security.Principal.WindowsPrincipal $identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$exclusionFix = @(
    ".\koru.bat setup --defender-exclusions   # opt in to faster trusted local builds",
    ".\scripts\setup\defender-exclusions.ps1 -WhatIf   # elevated: show what it would add",
    ".\scripts\setup\defender-exclusions.ps1           # elevated: repo, target dirs, cargo registry, SCCACHE_DIR"
)
# setup runs the script: here when this shell is elevated, otherwise in an
# elevated one that Windows asks to confirm. Declining is an answer, not a
# failure, and leaves the check as it was, so the next setup asks again.
#
# An unelevated shell cannot read the exclusions back, so a run that worked
# leaves a marker in this checkout's own git directory (a linked worktree has
# its own), naming the repository and the cache it covered. While both still
# match, setup does not ask again; a new SCCACHE_DIR, or a checkout elsewhere,
# does. An elevated setup reads the real list instead, and refreshes it.
$gitDir = (& git -C $repo rev-parse --absolute-git-dir 2>$null)
$marker = if ($gitDir) { Join-Path $gitDir "koru-defender-excluded" } else { $null }
$markerText = "{0}`n{1}" -f $repo, $cacheDir
function Test-Marker {
    $marker -and (Test-Path -LiteralPath $marker) -and
        ((Get-Content -Raw -LiteralPath $marker).Trim() -eq $markerText)
}
function Write-Marker {
    if ($marker) { [System.IO.File]::WriteAllText($marker, "$markerText`n") }
}
function Add-Exclusions {
    $script = Join-Path $repo "scripts\setup\defender-exclusions.ps1"
    Note "Defender stops scanning the repository and the build cache; scripts\setup\defender-exclusions.ps1 states the trade-off."
    try {
        if ($admin) {
            & $script
            return $?
        }
        Note "Windows asks to run it as administrator."
        $p = Start-Process powershell -Verb RunAs -WindowStyle Hidden -Wait -PassThru `
            -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$script`"")
        return ($p.ExitCode -eq 0)
    } catch {
        return $false
    }
}
if (-not $DefenderExclusions) {
    Note "Optional: for faster builds in a trusted checkout, consider koru.bat setup --defender-exclusions."
    Note "This disables Defender scanning of the repository, worktrees, Cargo sources and build cache; review scripts\setup\defender-exclusions.ps1 first."
} elseif ($admin) {
    $ex = $null
    try { $ex = @((Get-MpPreference).ExclusionPath) } catch { }
    if ($ex -eq $null) {
        Unknown "Could not read the Defender exclusion list" $null $exclusionFix
    } else {
        $missing = @()
        foreach ($p in @($repo, $cacheDir)) {
            $norm = $p.TrimEnd('\').ToLowerInvariant()
            $covered = $false
            foreach ($e in $ex) {
                if (-not $e) { continue }
                $en = $e.TrimEnd('\').ToLowerInvariant()
                if ($norm -eq $en -or $norm.StartsWith($en + '\')) { $covered = $true; break }
            }
            if (-not $covered) { $missing += $p }
        }
        if ($missing.Count -eq 0) {
            Pass "Defender excludes the repository and the sccache cache"
            if ($Apply) { Write-Marker }
        } elseif ($Apply -and (Add-Exclusions)) {
            Write-Marker
            Fixed ("Defender now excludes: {0}" -f ($missing -join ", "))
        } else {
            Warn ("Defender is scanning: {0}" -f ($missing -join ", ")) `
                "Every rlib, pdb and exe cargo writes is scanned as it is written: 20-40% of a Windows build." `
                $exclusionFix
        }
    }
} elseif (Test-Marker) {
    Pass "Defender exclusions were added by an earlier setup" `
        "This shell cannot read them back; an elevated koru.bat setup verifies them."
} elseif ($Apply -and (Add-Exclusions)) {
    Write-Marker
    Fixed "Defender exclusions added, in an elevated shell" `
        "This shell cannot read them back; an elevated koru.bat setup verifies them."
} else {
    Unknown "Defender exclusions cannot be verified from this shell" `
        "Reading the list needs administrator rights, as does changing it. Run this check elevated to verify, or run the script:" `
        $exclusionFix
}

# --------------------------------------------------------------- cargo jobs
$cs = Get-CimInstance Win32_ComputerSystem
$cores = [int]$cs.NumberOfLogicalProcessors
$ramGib = [math]::Round($cs.TotalPhysicalMemory / 1GB)
# Four GB per rustc is the working rule: the workspace's large units (the
# Store, the importers, iroh at opt-level 3) peak there, and a link phase over
# that ratio has killed full-suite runs for memory on a 31 GB machine.
$recommended = [math]::Max(2, [math]::Min($cores, [math]::Round($ramGib / 4)))
$configured = $null
foreach ($line in (Get-Content -LiteralPath (Join-Path $repo ".cargo\config.toml"))) {
    if ($line -match '^\s*jobs\s*=\s*(\d+)') { $configured = [int]$matches[1] }
}
$override = $env:CARGO_BUILD_JOBS
$effective = if ($override) { [int]$override } else { $configured }
$source = if ($override) { "CARGO_BUILD_JOBS" } else { ".cargo\config.toml" }
$setJobs = '[Environment]::SetEnvironmentVariable("CARGO_BUILD_JOBS", "{0}", "User")   # per machine; the tracked config stays as it is' -f $recommended
$machine = "{0} logical cores, {1} GB RAM" -f $cores, $ramGib
if ($effective -eq $null) {
    Unknown "cargo build jobs: no jobs = N in .cargo\config.toml and no CARGO_BUILD_JOBS" ("{0} fits this machine ({1})." -f $recommended, $machine) @($setJobs)
} elseif ($effective -gt $recommended -and $Apply) {
    Set-UserSetting "CARGO_BUILD_JOBS" ([string]$recommended)
    Fixed ("CARGO_BUILD_JOBS is now {0} (was {1}, from {2})" -f $recommended, $effective, $source) `
        ("At the user scope, for this machine ({0}); the tracked config stays as it is." -f $machine)
} elseif ($effective -gt $recommended) {
    Warn ("cargo build jobs = {0} ({1}); {2} fits this machine ({3})" -f $effective, $source, $recommended, $machine) `
        "More jobs than memory allows is a build that takes the desktop with it, or a link phase killed for memory." `
        @($setJobs)
} elseif ($effective -lt $recommended) {
    Pass ("cargo build jobs = {0} ({1})" -f $effective, $source) ("{0} would fit this machine ({1}); fewer is safe, only slower." -f $recommended, $machine)
} else {
    Pass ("cargo build jobs = {0} ({1}) fits this machine ({2})" -f $effective, $source, $machine)
}

# --------------------------------------------------------------- fonts
# The same sentinel fetch-fonts.ps1 uses to call a directory complete.
$sentinel = Join-Path $repo "assets\fonts\Inter.ttf"
function Test-Fonts { (Test-Path -LiteralPath $sentinel) -and ((Get-Item -LiteralPath $sentinel).Length -gt 0) }
# fetch-fonts.ps1 runs under 'Stop', so a failed download throws; that is a
# WARN below, not the end of the check.
function Get-Fonts {
    try { & (Join-Path $repo "scripts\setup\fetch-fonts.ps1") | Out-Null } catch { }
    return (Test-Fonts)
}
if (Test-Fonts) {
    Pass "bundled fonts are present in this worktree"
} elseif ($Apply -and (Get-Fonts)) {
    Fixed "bundled fonts fetched into this worktree"
} else {
    Warn "bundled fonts are missing from this worktree" `
        "They are gitignored and fetched per worktree. The app falls back to system faces, and the font-shaping tests fail." `
        @(".\koru.bat fonts")
}

# --------------------------------------------------------------- fetched code, skills
# koru.bat fetches the third-party code and installs the skills before it
# runs this, and before every other verb; these say whether that took.
$manifest = Join-Path $repo "third-party\fetch.manifest"
$stamp = Join-Path $repo "third-party\crates\.fetched"
if ((Test-Path -LiteralPath $stamp) -and
    ((Get-FileHash -LiteralPath $stamp).Hash -eq (Get-FileHash -LiteralPath $manifest).Hash)) {
    Pass "third-party code is fetched and matches third-party\fetch.manifest"
} else {
    Warn "third-party code is not fetched, or is older than third-party\fetch.manifest" `
        "Every cargo command needs it; any koru.bat verb fetches it." @(".\koru.bat setup")
}
$skillsIn = Get-ChildItem -Path (Join-Path $repo ".agents\skills\*\SKILL.md") -ErrorAction SilentlyContinue
$skillsLink = Get-Item -LiteralPath (Join-Path $repo ".claude\skills") -Force -ErrorAction SilentlyContinue
$linkType = if ($skillsLink) { $skillsLink.LinkType } else { $null }
$linked = $linkType -and (Get-ChildItem -Path (Join-Path $repo ".claude\skills\*\SKILL.md") -ErrorAction SilentlyContinue)
if ($skillsIn -and $linked -and $linkType -eq "SymbolicLink") {
    Pass "agent skills are installed (koruworks/skills), and .claude\skills links to them"
} elseif ($skillsIn -and $linked) {
    Pass ("agent skills are installed (koruworks/skills), and .claude\skills is a {0} to them" -f $linkType)
    Note "git worktree remove cannot delete a junction. With Developer Mode on, delete .claude\skills and run setup for a symlink."
} elseif ($skillsIn) {
    Note "Optional agent tooling needs repair: koru.bat setup --agent-skills."
} else {
    Note "Optional coding-agent skills and Git hooks: koru.bat setup --agent-skills (requires Node.js)."
}
# Behind koruworks/skills? .agents\check-skills.ps1 keeps the record and the
# agents ask at session start; here its one line, when it has one, joins the
# report.
$checkSkills = Join-Path $repo ".agents\check-skills.ps1"
if ($skillsIn -and (Test-Path -LiteralPath $checkSkills)) {
    foreach ($line in (& $checkSkills -Mode --now)) { if ($line) { Note $line } }
}
# docs\ is the team's private koru-docs submodule. Nothing builds from it, so
# not having it is a note, not a warning: an outside contributor never will.
if (Test-Path (Join-Path $repo "docs\README.md")) {
    Pass "docs\ is checked out (the koru-docs submodule)"
} else {
    Write-Host "  note   docs\ is not checked out: it is the team's private koru-docs repository" -ForegroundColor DarkGray
    Note "The build does not need it. With access: git submodule update --init docs"
}
$hooks = (& git -C $repo config --get core.hooksPath 2>$null)
if ($hooks -eq ".githooks") {
    Pass "core.hooksPath is .githooks, so a new worktree installs the skills"
} elseif ($hooks) {
    Note ("core.hooksPath is {0}; run setup --agent-skills in a new worktree if wanted." -f $hooks)
} else {
    Note "Optional worktree skill installation: koru.bat setup --agent-skills."
}

if ($script:restartServer -and (Get-Command sccache -ErrorAction SilentlyContinue)) {
    & sccache --stop-server 2>$null | Out-Null
    Note "sccache server stopped; the next cargo command starts one with the new settings."
}

Write-Host ""
if ($script:warnings -eq 0 -and $script:fixes -eq 0) {
    Write-Host "Nothing to change." -ForegroundColor Green
} elseif ($script:warnings -eq 0) {
    Write-Host ("Set up {0} thing(s); nothing left to change." -f $script:fixes) -ForegroundColor Green
} else {
    $hint = if ($Apply) { "each has its command above, and the next setup tries again." } else { "each has its command above, or run koru.bat setup." }
    Write-Host ("{0} thing(s) to change; {1}" -f $script:warnings, $hint) -ForegroundColor Yellow
}
Write-Host "README.md, Build and run from a fresh clone, lists what this machine needs."
exit $script:warnings
