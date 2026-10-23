# One measurement session on this Windows PC (dry-run, calibration or confirmatory).
# Windows PowerShell 5.1+, native, no Docker.
#
#   powershell -ExecutionPolicy Bypass -File scripts\session.ps1 -Mode dryrun -Hours 8
#   powershell -ExecutionPolicy Bypass -File scripts\session.ps1 -Mode calibrate -Hours 8 -MaxSpendLamports 2500000 -Offsets "-300,-100,0,100"
#   powershell -ExecutionPolicy Bypass -File scripts\session.ps1 -Mode measure -Hours 8 -MaxSpendLamports 13000000 -Prereg <commit> -Allocation analysis\sim\results\alloc_v2.json
#   powershell -ExecutionPolicy Bypass -File scripts\session.ps1 -Resume        # continue the open session
#   powershell -ExecutionPolicy Bypass -File scripts\session.ps1 -PreflightOnly
#   powershell -ExecutionPolicy Bypass -File scripts\session.ps1 -Reset         # close a session that will not be resumed
#
# What it guarantees:
# - Copied binary. target\release\sender.exe is copied once per session to
#   run\bin\sender-<sha12>.exe and every run of the session uses that copy, so a rebuild during the
#   session cannot change or lock the running binary. Its SHA-256 is in the session state and in
#   every run header (run.json "host", crates/sender/src/host.rs).
# - No idle sleep while the script runs (SetThreadExecutionState). A sleep anyway shows up as a
#   host_stall gap in the run.
# - Resume. A session is a fixed UTC interval [start, start + hours), kept in
#   data\sessions\current.json. If sender exits non-zero it is restarted after 30 s with the
#   remaining time; live sessions first run `sender collect`, which resolves the open
#   reservations. If the PC crashes, loses power or reboots, a .cmd in the user's Startup folder
#   runs `session.ps1 -Resume` at the next logon. It continues the SAME session until its end,
#   and the planner continues the allocation from the ledger (runner.rs prior_plan_state). The
#   Startup entry is removed when the session ends.
# - Gaps logged with UTC timestamps, never filled: data\sessions\<id>\events.jsonl has start,
#   resume, down (heartbeat to restart: crash, power loss, reboot), run_exit, sender_down (restart
#   pause), collect, stopped, done. Inside a run, sender logs host_stall / slot_feed / rpc_error
#   to <run>\gaps.jsonl.
# - Pre-flight (data\sessions\<id>\preflight.json): warns if the power plan lets the PC sleep or
#   hibernate, if Windows Update is waiting for a restart, if the default route is Wi-Fi, if on
#   battery, if the clock is not synced, if disk space is low; records platform, OS build and
#   binary hash. Live modes show the warnings and need the operator to type "yes".
# - Coverage: at the end, slwlab.merge_dryrun writes data\sessions\<id>\coverage.{json,md}: covered
#   seconds, clock error and windows per UTC hour for this session's runs, gaps flagged.
param(
    [ValidateSet('dryrun', 'calibrate', 'measure')][string]$Mode = 'dryrun',
    [double]$Hours = 8,
    [long]$MaxSpendLamports = 0,
    [string]$Prereg = '',
    [string]$Allocation = '',
    [string]$Offsets = '',
    [switch]$Resume,
    [switch]$Reset,
    [switch]$PreflightOnly,
    [switch]$NoStartupEntry,
    # tests only: another sender executable and a session length in seconds
    [string]$Exe = 'target\release\sender.exe',
    [int]$TestSecs = 0
)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)
$Root = (Get-Location).Path
$Utf8 = New-Object System.Text.UTF8Encoding($false)
$SessDir = Join-Path $Root 'data\sessions'
$Current = Join-Path $SessDir 'current.json'
$Startup = Join-Path ([Environment]::GetFolderPath('Startup')) 'slwlab-session-resume.cmd'
New-Item -ItemType Directory -Force $SessDir | Out-Null

function NowUnix { [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() }
function Iso([long]$u) { [DateTimeOffset]::FromUnixTimeSeconds($u).UtcDateTime.ToString("yyyy-MM-dd'T'HH:mm:ss'Z'") }
function Stamp([long]$u) { [DateTimeOffset]::FromUnixTimeSeconds($u).UtcDateTime.ToString("yyyyMMdd'T'HHmmss'Z'") }
function WriteJson($path, $obj) { [IO.File]::WriteAllText($path, ($obj | ConvertTo-Json -Depth 6), $Utf8) }
function Event($kind, [hashtable]$extra = @{}) {
    $o = [ordered]@{ ts = (Iso (NowUnix)); event = $kind }
    foreach ($k in $extra.Keys) { $o[$k] = $extra[$k] }
    [IO.File]::AppendAllText($script:Events, (($o | ConvertTo-Json -Compress) + "`n"), $Utf8)
}
function DotEnv($key) {
    if (-not (Test-Path '.env')) { return '' }
    foreach ($l in [IO.File]::ReadAllLines((Join-Path $Root '.env'))) {
        $t = $l.Trim()
        if ($t.StartsWith('#') -or -not $t.Contains('=')) { continue }
        $i = $t.IndexOf('=')
        if ($t.Substring(0, $i).Trim() -eq $key) { return $t.Substring($i + 1).Trim().Trim('"') }
    }
    return ''
}

# ---------------------------------------------------------------- pre-flight
function PowerIndex($sub, $setting) {
    # Locale-independent: the last two hex values are the AC and DC indices (seconds; 0 = never).
    $out = (powercfg /query SCHEME_CURRENT $sub $setting) 2>$null | Out-String
    $hex = [regex]::Matches($out, '0x[0-9a-fA-F]{8}') | ForEach-Object { $_.Value }
    if ($hex.Count -lt 2) { return $null }
    return @{ ac = [Convert]::ToInt64($hex[-2], 16); dc = [Convert]::ToInt64($hex[-1], 16) }
}
function Preflight {
    $w = New-Object System.Collections.ArrayList
    $info = [ordered]@{}
    $sleep = PowerIndex 'SUB_SLEEP' 'STANDBYIDLE'
    $hib = PowerIndex 'SUB_SLEEP' 'HIBERNATEIDLE'
    $info.sleep_after_secs = $sleep; $info.hibernate_after_secs = $hib
    if ($null -eq $sleep) { [void]$w.Add('power plan: could not read the sleep timeout') }
    elseif ($sleep.ac -gt 0) { [void]$w.Add("power plan: PC sleeps after $($sleep.ac) s idle on AC (the script blocks idle sleep while it runs; set 'Never' to be safe)") }
    if ($null -ne $hib -and $hib.ac -gt 0) { [void]$w.Add("power plan: PC hibernates after $($hib.ac) s idle on AC") }
    $pending = @()
    if (Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired') { $pending += 'WindowsUpdate RebootRequired' }
    if (Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending') { $pending += 'CBS RebootPending' }
    try {
        $pfr = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager' -Name PendingFileRenameOperations -ErrorAction Stop).PendingFileRenameOperations
        if ($pfr) { $pending += 'PendingFileRenameOperations' }
    } catch {}
    $info.restart_pending = $pending
    if ($pending.Count -gt 0) { [void]$w.Add("Windows Update / servicing is waiting for a restart ($($pending -join ', ')): restart BEFORE the session, or it may restart during it") }
    try {
        $route = Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction Stop | Sort-Object { $_.RouteMetric + $_.InterfaceMetric } | Select-Object -First 1
        $nic = Get-NetAdapter -InterfaceIndex $route.ifIndex -ErrorAction Stop
        $info.network = [ordered]@{ name = $nic.Name; description = $nic.InterfaceDescription; media = "$($nic.PhysicalMediaType)"; link = "$($nic.LinkSpeed)" }
        if ("$($nic.PhysicalMediaType)" -match '802\.11|Wireless' -or $nic.InterfaceDescription -match 'Wi-?Fi|Wireless|802\.11') {
            [void]$w.Add("network: the default route uses Wi-Fi ($($nic.InterfaceDescription)); use Ethernet for stable timing")
        }
    } catch { [void]$w.Add('network: could not determine the default-route adapter') }
    try {
        $bat = Get-CimInstance Win32_Battery -ErrorAction Stop
        if ($bat) {
            $info.battery_status = $bat.BatteryStatus
            if ($bat.BatteryStatus -eq 1) { [void]$w.Add('power: running on battery') }
        }
    } catch {}
    # Locale-independent: the Windows Time service must run, and its source must not be the
    # local CMOS clock (the source line is the one value w32tm does not translate).
    $svc = Get-Service w32time -ErrorAction SilentlyContinue
    $info.time_service = if ($svc) { "$($svc.Status)" } else { 'missing' }
    $src = ((w32tm /query /source) 2>$null | Out-String).Trim()
    $info.time_source = $src
    if (-not $svc -or $svc.Status -ne 'Running') {
        [void]$w.Add("clock: the Windows Time service is $($info.time_service), so the clock is not being synced (start it: 'net start w32time' as admin, then 'w32tm /resync')")
    } elseif ($src -match 'Local CMOS Clock|Free-running') {
        [void]$w.Add("clock: time source is '$src', not a time server")
    }
    $drive = Get-PSDrive -Name ($Root.Substring(0, 1)) -ErrorAction SilentlyContinue
    if ($drive) {
        $info.free_gb = [math]::Round($drive.Free / 1GB, 1)
        if ($drive.Free -lt 5GB) { [void]$w.Add("disk: only $($info.free_gb) GB free on $($drive.Name):") }
    }
    $platform = $env:SLWLAB_PLATFORM
    if (-not $platform) { $platform = DotEnv 'SLWLAB_PLATFORM' }
    $info.platform = $platform
    $info.os_build = [Environment]::OSVersion.Version.ToString()
    try { $info.os_ubr = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' -Name UBR -ErrorAction Stop).UBR } catch {}
    if (-not $platform) { [void]$w.Add('platform: SLWLAB_PLATFORM is not set in .env (required for calibration and confirmatory sessions)') }
    return @{ info = $info; warnings = @($w) }
}

# ---------------------------------------------------------------- reset / preflight-only
if ($Reset) {
    if (Test-Path $Current) {
        $st = Get-Content $Current -Raw | ConvertFrom-Json
        $script:Events = Join-Path $SessDir "$($st.id)\events.jsonl"
        Event 'reset' @{ detail = 'session closed by operator before its end' }
        Move-Item $Current (Join-Path $SessDir "$($st.id)\state.json") -Force
    }
    if (Test-Path $Startup) { Remove-Item $Startup -Force }
    Write-Host 'no open session'
    exit 0
}
if ($PreflightOnly) {
    $p = Preflight
    $p.info | ConvertTo-Json -Depth 4 | Write-Host
    if ($p.warnings.Count) { $p.warnings | ForEach-Object { Write-Warning $_ } } else { Write-Host 'pre-flight: no warnings' }
    exit 0
}

# ---------------------------------------------------------------- open or resume the session
$live = $false
if (Test-Path $Current) {
    $st = Get-Content $Current -Raw | ConvertFrom-Json
    if (-not $Resume) {
        Write-Error "session $($st.id) ($($st.mode)) is open until $(Iso $st.end). Use -Resume to continue it or -Reset to close it."
        exit 1
    }
    $script:Events = Join-Path $SessDir "$($st.id)\events.jsonl"
    $hb = Join-Path $SessDir "$($st.id)\heartbeat"
    $t = NowUnix
    if (Test-Path $hb) {
        $last = [long](Get-Content $hb -Raw)
        if ($t - $last -gt 90) {
            Event 'down' @{ from = (Iso $last); to = (Iso $t); secs = ($t - $last); detail = 'no heartbeat: script not running (crash, power loss, reboot, closed window)' }
        }
    }
    Event 'resume' @{ left_secs = ($st.end - $t) }
    Write-Host "resuming $($st.mode) session $($st.id) until $(Iso $st.end)"
} else {
    if ($Resume) { Write-Host 'no open session to resume'; if (Test-Path $Startup) { Remove-Item $Startup -Force }; exit 0 }
    if ($Hours -gt 10) { Write-Error 'sessions are at most 10 h (README: Sessions)'; exit 1 }
    if (-not (Test-Path $Exe)) { Write-Error "build first: cargo build --release --locked -p slwlab-sender ($Exe missing)"; exit 1 }
    if (-not (Test-Path '.env')) { Write-Error ".env with RPC_URL / RPC_WS_URL not found in $Root"; exit 1 }
    $live = $Mode -ne 'dryrun'
    if ($live -and $MaxSpendLamports -le 0) { Write-Error '-MaxSpendLamports is required for calibrate/measure'; exit 1 }
    if ($Mode -eq 'measure' -and (-not $Prereg -or -not $Allocation)) { Write-Error 'measure needs -Prereg <prereg-v1 commit> and -Allocation <file>'; exit 1 }
    $start = NowUnix
    $id = Stamp ($start - 1)
    $dir = Join-Path $SessDir $id
    New-Item -ItemType Directory -Force $dir | Out-Null
    $script:Events = Join-Path $dir 'events.jsonl'
    $pf = Preflight
    WriteJson (Join-Path $dir 'preflight.json') ([ordered]@{ ts = (Iso $start); info = $pf.info; warnings = $pf.warnings })
    foreach ($m in $pf.warnings) { Write-Warning $m }
    if ($live) {
        if (-not $pf.info.platform) { Write-Error 'set SLWLAB_PLATFORM in .env first (one id for this PC, used by calibration and every confirmatory session)'; exit 1 }
        Write-Host "LIVE $Mode session: $Hours h, cap $MaxSpendLamports lamports for the whole session. Pre-flight warnings: $($pf.warnings.Count)."
        $ans = Read-Host 'Type yes to send real transactions'
        if ($ans -ne 'yes') { Write-Host 'aborted'; Remove-Item $dir -Recurse -Force; exit 1 }
    }
    # Copy the binary once; every run of the session uses the copy.
    New-Item -ItemType Directory -Force (Join-Path $Root 'run\bin') | Out-Null
    $sha = (Get-FileHash $Exe -Algorithm SHA256).Hash.ToLower()
    $ext = [IO.Path]::GetExtension($Exe)
    $copy = Join-Path $Root "run\bin\sender-$($sha.Substring(0, 12))$ext"
    if (-not (Test-Path $copy)) { Copy-Item $Exe $copy }
    $secs = if ($TestSecs -gt 0) { $TestSecs } else { [long]($Hours * 3600) }
    $st = [pscustomobject][ordered]@{
        id = $id; mode = $Mode; start = $start; end = ($start + $secs); exe = $copy; exe_sha256 = $sha
        max_spend_lamports = $MaxSpendLamports; prereg = $Prereg; allocation = $Allocation; offsets = $Offsets
        confirmed_live = $live; platform = $pf.info.platform
    }
    WriteJson $Current $st
    Event 'start' @{ end = (Iso $st.end); mode = $Mode; exe_sha256 = $sha; warnings = $pf.warnings.Count }
    Write-Host "$Mode session $id until $(Iso $st.end); binary $copy"
}
$dir = Join-Path $SessDir $st.id
$live = $st.mode -ne 'dryrun'

# The copy must still be the binary the session started with.
if (-not (Test-Path $st.exe) -or (Get-FileHash $st.exe -Algorithm SHA256).Hash.ToLower() -ne $st.exe_sha256) {
    Event 'stopped' @{ detail = 'session binary missing or changed' }
    Write-Error "session binary $($st.exe) is missing or changed; not resuming"
    exit 1
}
if ($st.platform) { $env:SLWLAB_PLATFORM = $st.platform }
if (-not $NoStartupEntry) {
    $cmd = "@echo off`r`nstart `"slwlab session`" powershell -NoExit -ExecutionPolicy Bypass -File `"$Root\scripts\session.ps1`" -Resume`r`n"
    [IO.File]::WriteAllText($Startup, $cmd, [Text.Encoding]::ASCII)
}

Add-Type -Namespace Slwlab -Name Power -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint esFlags);' -ErrorAction SilentlyContinue
# ES_CONTINUOUS | ES_SYSTEM_REQUIRED: no idle sleep while this process lives.
[void][Slwlab.Power]::SetThreadExecutionState([uint32]'0x80000001')

# Heartbeat: lets the next start measure how long nothing was running.
$hbFile = Join-Path $dir 'heartbeat'
$hbJob = Start-Job -ArgumentList $hbFile -ScriptBlock {
    param($f)
    while ($true) { [IO.File]::WriteAllText($f, [string][DateTimeOffset]::UtcNow.ToUnixTimeSeconds()); Start-Sleep -Seconds 30 }
}
[IO.File]::WriteAllText($hbFile, [string](NowUnix))

# Session spend so far: every reservation made since the session started (an upper bound on the
# fees; reservations are settled later). Each live run gets only the remaining session cap.
function SessionReserved {
    $p = Join-Path $Root 'data\ledger.jsonl'
    if (-not (Test-Path $p)) { return 0 }
    $sum = [long]0
    $from = [DateTimeOffset]::FromUnixTimeSeconds($st.start).UtcDateTime
    foreach ($l in [IO.File]::ReadAllLines($p)) {
        if (-not $l.Contains('"kind":"reserve"')) { continue }
        try { $e = $l | ConvertFrom-Json } catch { continue }
        if ([DateTime]::Parse($e.ts).ToUniversalTime() -ge $from) { $sum += [long]$e.amount }
    }
    return $sum
}

$done = $false
$n = @(Get-Content $script:Events -ErrorAction SilentlyContinue | Where-Object { $_ -match '"event":"run_exit"' }).Count
try {
    if ($live -and $Resume) {
        & $st.exe collect
        Event 'collect' @{ exit_code = $LASTEXITCODE; detail = 'resolve reservations left open by the interrupted run' }
    }
    while ($true) {
        $t0 = NowUnix
        $left = $st.end - $t0
        if ($left -lt 120) { break }
        $env:CONFIRM_LIVE = ''
        switch ($st.mode) {
            'dryrun' { $a = @('dry-run', '--duration-secs', $left) }
            default {
                $cap = $st.max_spend_lamports - (SessionReserved)
                if ($cap -le 0) { Event 'cap_reached' @{ reserved = (SessionReserved) }; break }
                $env:CONFIRM_LIVE = 'yes'
                $a = @('live', '--purpose', $st.mode, '--duration-secs', $left, '--max-spend-lamports', $cap)
                if ($st.offsets) { $a += @('--offsets', $st.offsets) }
                if ($st.mode -eq 'measure') { $a += @('--prereg', $st.prereg, '--allocation', $st.allocation) }
            }
        }
        if ($st.mode -ne 'dryrun' -and $env:CONFIRM_LIVE -ne 'yes') { break }
        & $st.exe @a
        $code = $LASTEXITCODE
        $env:CONFIRM_LIVE = ''
        $t1 = NowUnix
        $n++
        Event 'run_exit' @{ run = $n; exit_code = $code; ran_secs = ($t1 - $t0); left_secs = $left }
        if ($code -ne 0) {
            Write-Host "sender exited with $code after $($t1 - $t0) s; restarting in 30 s"
            Start-Sleep -Seconds 30
            if ($live) {
                & $st.exe collect
                Event 'collect' @{ exit_code = $LASTEXITCODE }
            }
            Event 'sender_down' @{ from = (Iso $t1); to = (Iso (NowUnix)); secs = ((NowUnix) - $t1); detail = "exit $code, restart pause" }
        }
    }
    if ($live) {
        & $st.exe collect
        Event 'collect' @{ exit_code = $LASTEXITCODE; detail = 'end of session' }
    }
    $done = $true
} finally {
    $env:CONFIRM_LIVE = ''
    Stop-Job $hbJob -ErrorAction SilentlyContinue; Remove-Job $hbJob -Force -ErrorAction SilentlyContinue
    if (-not $done) { Event 'stopped' @{ detail = 'script interrupted (Ctrl+C or window closed); -Resume continues' } }
}

# ---------------------------------------------------------------- close: coverage by UTC hour
$py = Join-Path $Root '.venv\Scripts\python.exe'
if (Test-Path $py) {
    Push-Location analysis
    & $py -m slwlab.merge_dryrun --data ..\data --since (Stamp ($st.start - 1)) --until (Stamp ($st.end + 120)) --out "..\data\sessions\$($st.id)\coverage.json"
    Pop-Location
}
Event 'done'
Move-Item $Current (Join-Path $dir 'state.json') -Force
Remove-Item $hbFile -ErrorAction SilentlyContinue
if (Test-Path $Startup) { Remove-Item $Startup -Force }
[void][Slwlab.Power]::SetThreadExecutionState([uint32]'0x80000000')
Write-Host "session $($st.id) done. events: $($script:Events) ; coverage: data\sessions\$($st.id)\coverage.md"
