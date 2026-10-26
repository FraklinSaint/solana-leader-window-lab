# Read-only monitor of the newest session in data\sessions (RUNBOOK_MAINNET S7b step 5).
# Run in a second window: powershell -NoProfile -File scripts\monitor.ps1
cd C:\Users\user\solana-leader-window-lab
$sd = Get-ChildItem data\sessions -Directory | Sort-Object Name | Select-Object -Last 1
$ss = Get-Content "$($sd.FullName)\session.json" -Raw | ConvertFrom-Json
$ci = [Globalization.CultureInfo]::InvariantCulture; $ad = [Globalization.DateTimeStyles]'AdjustToUniversal,AssumeUniversal'
function U($s) { [DateTime]::Parse($s, $ci, $ad) }
$now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds(); $t0 = [int64]$ss.start_unix; $t1 = [int64]$ss.end_unix; $cap = [int64]$ss.max_spend_lamports
$el = [Math]::Min([Math]::Max($now - $t0, 0), $t1 - $t0); $line = [int64]($cap * $el / ($t1 - $t0))
$purpose = if ($ss.mode -eq 'measure') { 'measure' } else { 'calibrate' }
$res = Get-Content data\ledger.jsonl | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object { $_.kind -eq 'reserve' -and $_.purpose -eq $purpose -and (U $_.ts) -ge (U $ss.start_utc) -and (U $_.ts) -le (U $ss.end_utc).AddMinutes(10) }
$spent = ($res | Measure-Object amount -Sum).Sum; if (-not $spent) { $spent = 0 }
"SESSION $($ss.id) mode=$($ss.mode) $($ss.start_utc) -> $($ss.end_utc)   now $([DateTime]::UtcNow.ToString('HH:mm:ss'))Z   elapsed $([Math]::Round($el/3600,2)) h"
"SPEND   reserved $spent of cap $cap | pacing line now $line | ratio $([Math]::Round($spent / [Math]::Max($line,1), 2)) (1.00 = on the line; the planner stops above line + 1 window)"
"WINDOWS per UTC hour (distinct windows with a reservation):"
$res | Group-Object window_id | ForEach-Object { [pscustomobject]@{ h = (U ($_.Group[0].ts)).ToString('MM-dd HH') + 'Z'; lamports = ($_.Group | Measure-Object amount -Sum).Sum } } | Group-Object h | ForEach-Object { "  {0}  windows {1,3}  lamports {2,9}" -f $_.Name, $_.Count, ($_.Group | Measure-Object lamports -Sum).Sum }
$ref = [Math]::Min($now, $t1); $bins = @(0, 0, 0, 0)
$res | Group-Object window_id | ForEach-Object { $age = $ref - ([DateTimeOffset](U $_.Group[0].ts)).ToUnixTimeSeconds(); if ($age -ge 0 -and $age -lt 3600) { $bins[[int][Math]::Floor($age / 900)] += 1 } }
"LAST 60 MIN in 15-min bins (newest first): $($bins -join ' ')   (a 0 inside a running session = a gap)"
$ev = Get-Content "$($sd.FullName)\events.jsonl" | ForEach-Object { $_ | ConvertFrom-Json }
"EVENTS  " + (($ev | Group-Object event | Sort-Object Name | ForEach-Object { "$($_.Name)=$($_.Count)" }) -join ' ')
"LAST EVENT $(($ev | Select-Object -Last 1).event) at $(($ev | Select-Object -Last 1).ts) | last ledger entry $((Get-Content data\ledger.jsonl -Tail 1 | ConvertFrom-Json).ts) | heartbeat file written $((Get-Item "$($sd.FullName)\rtt.jsonl").LastWriteTime.ToUniversalTime().ToString('HH:mm:ss'))Z"
$runs = Get-ChildItem data\logs -Directory -Filter 'live-*' | Where-Object { $_.Name -match '^live-(\d{8}T\d{6}Z)$' -and $Matches[1] -ge $ss.id -and (U ($Matches[1].Insert(13,':').Insert(11,':').Insert(6,'-').Insert(4,'-'))) -le (U $ss.end_utc).AddMinutes(10) }
$skips = @{}; $cd = @(0, 0)
foreach ($r in $runs) {
  $pl = "$($r.FullName)\plan.jsonl"
  if (Test-Path $pl) { Select-String -Path $pl -SimpleMatch '"skip"' | ForEach-Object { $k = ($_.Line | ConvertFrom-Json).skip.reason -replace '\d+', '#'; $skips[$k] += 1 } }
  $sj = "$($r.FullName)\summary.json"
  if (Test-Path $sj) { $s = Get-Content $sj -Raw | ConvertFrom-Json; $cd[0] += [int]$s.conn_dead_reconnected; $cd[1] += [int]$s.conn_dead_skipped }
}
"WORKER RUNS $(@($runs).Count) (more than 1 per few hours = restarts) | restart pauses: $(@($ev | Where-Object { $_.event -eq 'worker_down' }).Count)"
"DEAD CONNECTION  conn_dead events $(@($ev | Where-Object { $_.event -eq 'conn_dead' }).Count) | finished runs: reconnected $($cd[0]), skipped $($cd[1])"
"SKIP REASONS (plan.jsonl):"; $skips.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 8 | ForEach-Object { "  {0,6}  {1}" -f $_.Value, $_.Key }
"DISK  C: free {0:N1} GB   D: free {1:N1} GB" -f ((Get-PSDrive C).Free / 1GB), ((Get-PSDrive D -ErrorAction SilentlyContinue).Free / 1GB)
