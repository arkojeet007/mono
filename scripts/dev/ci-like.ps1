# SPDX-License-Identifier: MPL-2.0

param(
    # Limit the run to these packages (cargo -p names). Empty: the workspace.
    [string[]]$Package = @(),
    # Run only test binaries whose target name matches this (a regex).
    [string]$Target = '',
    # How many times each binary is run, one after another.
    [int]$Rounds = 3,
    # How many logical cores every run, and everything it starts, may use.
    [int]$Cores = 4,
    [int]$TimeoutMinutes = 15,
    [string]$Out = (Join-Path ([System.IO.Path]::GetTempPath()) 'koru-ci-like')
)

# Run the test suite the way CI's Windows runner experiences it, before CI
# does: pinned to four logical cores, with no GPU warm-up, a few times over.
#
# A developer machine passes tests that fail on CI. The runner has four vCPUs
# and no GPU; a workstation has many cores and a GPU, so a test that waits
# 160 ms for something to still be running, or 5 s for a child process to
# answer, never misses here and misses there. Pinning every test process to
# -Cores reproduces that: the affinity is inherited by the Agents, workers and
# fixtures a test starts, so the whole stack competes for the same cores the
# way it does on the runner. KORU_NO_GPU_WARMUP=1 is set exactly as ci.yml
# sets it.
#
# Each binary runs from its package directory, as `cargo test` runs it. A run
# that passes leaves nothing behind; a failing run's output is kept under
# -Out, and the summary names every test that failed and how often.
#
#   scripts/dev/ci-like.ps1                          # the whole workspace
#   scripts/dev/ci-like.ps1 -Package koru-ai-server -Rounds 20
#   scripts/dev/ci-like.ps1 -Package koru-agent -Target '^sessions_ipc$'

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$scope = if ($Package.Count -gt 0) { $Package | ForEach-Object { '-p', $_ } } else { @('--workspace') }

Write-Host "building test binaries ($($scope -join ' '))"
Push-Location $repo
try {
    # Cargo's progress goes to stderr, which Windows PowerShell would turn
    # into a terminating error under 'Stop'.
    $ErrorActionPreference = 'Continue'
    $artifacts = & cargo test @scope --locked --no-run --message-format=json 2>$null
} finally {
    $ErrorActionPreference = 'Stop'
    Pop-Location
}
if ($LASTEXITCODE -ne 0) { throw "cargo test --no-run failed ($LASTEXITCODE); run it directly to see why" }

$binaries = @(foreach ($line in $artifacts) {
    if (-not $line.StartsWith('{')) { continue }
    $message = $line | ConvertFrom-Json
    if ($message.reason -ne 'compiler-artifact' -or -not $message.profile.test -or -not $message.executable) { continue }
    if ($Target -and $message.target.name -notmatch $Target) { continue }
    [pscustomobject]@{
        Exe   = $message.executable
        Dir   = Split-Path -Parent $message.manifest_path
        Label = '{0} {1} {2}' -f (Split-Path -Leaf (Split-Path -Parent $message.manifest_path)), $message.target.kind[0], $message.target.name
    }
})
if (-not $binaries) { throw 'no test binaries matched' }

Remove-Item -Recurse -Force $Out -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $Out | Out-Null
$env:KORU_NO_GPU_WARMUP = '1'
$mask = [IntPtr]([long][Math]::Pow(2, $Cores) - 1)
$summary = @()
$index = 0

foreach ($binary in $binaries) {
    $index++
    $failedRuns = 0
    $failures = @{}
    foreach ($round in 1..$Rounds) {
        $log = Join-Path $Out (($binary.Label -replace '[^\w.-]', '_') + ".r$round.log")
        $env:CARGO_MANIFEST_DIR = $binary.Dir
        $process = Start-Process -FilePath $binary.Exe -WorkingDirectory $binary.Dir -NoNewWindow -PassThru `
            -RedirectStandardOutput $log -RedirectStandardError "$log.err"
        # Reading Handle now is what keeps ExitCode readable once it exits.
        $null = $process.Handle
        try { $process.ProcessorAffinity = $mask } catch {}
        $timedOut = -not $process.WaitForExit($TimeoutMinutes * 60000)
        if ($timedOut) { & taskkill /T /F /PID $process.Id 2>$null | Out-Null }
        $text = [string](Get-Content $log -Raw -ErrorAction SilentlyContinue)
        $named = [regex]::Matches($text, '(?m)^test (.+?) \.\.\. FAILED') | ForEach-Object { $_.Groups[1].Value }
        if ($timedOut -or $process.ExitCode -ne 0 -or $named) {
            $failedRuns++
            if ($timedOut) { $named = @("(timed out after $TimeoutMinutes min)") }
            elseif (-not $named) { $named = @("(exit $($process.ExitCode) without a failed test)") }
            foreach ($name in $named) { $failures[$name] = 1 + [int]$failures[$name] }
        } else {
            Remove-Item $log, "$log.err" -ErrorAction SilentlyContinue
        }
    }
    $state = if ($failedRuns) { "$failedRuns of $Rounds runs failed" } else { 'ok' }
    Write-Host ("[{0}/{1}] {2}: {3}" -f $index, $binaries.Count, $binary.Label, $state)
    if ($failedRuns) {
        $summary += [pscustomobject]@{ Label = $binary.Label; Failed = $failedRuns; Tests = $failures }
    }
}

Write-Host ''
if (-not $summary) {
    Write-Host "every binary passed $Rounds runs on $Cores cores"
    exit 0
}
Write-Host "failures (logs in $Out):"
foreach ($entry in $summary) {
    Write-Host ("  {0}: {1} of {2} runs" -f $entry.Label, $entry.Failed, $Rounds)
    foreach ($test in $entry.Tests.GetEnumerator() | Sort-Object Value -Descending) {
        Write-Host ("    {0}x {1}" -f $test.Value, $test.Key)
    }
}
exit 1
