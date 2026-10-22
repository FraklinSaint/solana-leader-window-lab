# 24 h mainnet dry-run (never sends). Windows PowerShell 5.1+, native, no Docker.
#
#   powershell -ExecutionPolicy Bypass -File scripts\dryrun24h.ps1
#
# - Endpoints come from .env in the repo root (RPC_URL, RPC_WS_URL), read by sender itself.
# - Resilient: if sender exits (crash, network outage at start-up), it is restarted after
#   30 s with the remaining duration. Every restart goes to data/logs/dryrun24h-<stamp>.restarts.jsonl.
#   Inside a run, WebSocket drops reconnect, RPC outages are retried, and host sleep, feed
#   stalls and RPC errors are logged to <run>/gaps.jsonl. They are not hidden.
# - -KeepAwake (default on) asks Windows not to idle-sleep while the script runs. The run
#   does not depend on it: a sleep shows up as a host_stall gap.
# - At the end, all runs of this campaign are merged into
#   data/exploratory/dryrun24h-<stamp>/clock_hourly.json (prediction error by UTC hour).
param(
    [double]$Hours = 24,
    [bool]$KeepAwake = $true
)
$ErrorActionPreference = 'Continue'
Set-Location (Split-Path -Parent $PSScriptRoot)

$exe = 'target\release\sender.exe'
if (-not (Test-Path $exe)) { Write-Error "build first: cargo build --release -p slwlab-sender"; exit 1 }
if (-not (Test-Path '.env')) { Write-Error ".env with RPC_URL / RPC_WS_URL not found in $(Get-Location)"; exit 1 }

if ($KeepAwake) {
    Add-Type -Namespace Slwlab -Name Power -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint esFlags);'
    # ES_CONTINUOUS | ES_SYSTEM_REQUIRED: no idle sleep while this process lives.
    [void][Slwlab.Power]::SetThreadExecutionState([uint32]'0x80000001')
}

$start = (Get-Date).ToUniversalTime()
$end = $start.AddHours($Hours)
# One second earlier, so the first run's directory (stamped a moment later) is included.
$stamp = $start.AddSeconds(-1).ToString("yyyyMMdd'T'HHmmss'Z'")
New-Item -ItemType Directory -Force 'data\logs' | Out-Null
$restarts = "data\logs\dryrun24h-$stamp.restarts.jsonl"
Write-Host "dry-run campaign $stamp until $($end.ToString('u')) (nothing is sent)"

$n = 0
while ($true) {
    $left = [int]($end - (Get-Date).ToUniversalTime()).TotalSeconds
    if ($left -lt 120) { break }
    $t0 = (Get-Date).ToUniversalTime()
    & $exe dry-run --duration-secs $left
    $code = $LASTEXITCODE
    $ran = [int]((Get-Date).ToUniversalTime() - $t0).TotalSeconds
    $n++
    $line = '{{"ts":"{0}","run":{1},"exit_code":{2},"ran_secs":{3},"left_secs":{4}}}' -f `
        (Get-Date).ToUniversalTime().ToString('o'), $n, $code, $ran, $left
    # UTF-8 without BOM (PowerShell 5.1's -Encoding utf8 writes one).
    [IO.File]::AppendAllText((Join-Path (Get-Location) $restarts), "$line`n")
    if ($code -ne 0) {
        Write-Host "sender exited with $code after $ran s; restarting in 30 s"
        Start-Sleep -Seconds 30
    }
}

$out = "data\exploratory\dryrun24h-$stamp\clock_hourly.json"
& $exe clock-report --since $stamp --out $out
Write-Host "done. hourly report: $out ; restarts: $restarts"
