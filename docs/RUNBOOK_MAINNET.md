# MAINNET calibration runbook (operator, Windows PowerShell)

Written 2026-10-25 from the code at the commit that adds this file. Every command, flag,
Expect and STOP line below comes from the source (`crates/sender/src/main.rs`, `session.rs`,
`funding.rs`, `keys.rs`, `preflight.rs`, `netmon.rs`, `analysis/slwlab/calibrate.py`,
`scripts/verify_manifest.py`) or from README. Anything the code does not settle is marked
**unverified**. Nothing here was run when it was written. The operator runs every live command;
Claude does not.

Money, from `crates/common/src/limits.rs`: hard ceiling 50,000,000 lamports; calibration
budget 5,000,000; funding/sweep reserve 5,000,000. `--max-spend-lamports` above the ceiling is
rejected, not clamped. Locked rent in payers is not spend.

Rules that apply everywhere:
- Type commands in the PowerShell window. Do not add scripts; no Add-Type, DllImport, encoded
  commands, Invoke-Expression, hidden windows or autostart entries (`test_script_hygiene.py`).
- Never print `.env` (it holds the RPC key). Never paste `secrets\` contents anywhere.
- `CONFIRM_LIVE` is set only in the window that runs a live command, and removed afterwards.
- **STOP means: do not run the next step.** Tell Claude the exact output.

---

## A) Setup

### A1. Window and environment

Open a NEW PowerShell window (not the one used for devnet), then:

```powershell
cd C:\Users\user\solana-leader-window-lab
Remove-Item Env:SLWLAB_ENV_FILE, Env:RPC_URL, Env:RPC_WS_URL, Env:MASTER_KEYPAIR, Env:SLWLAB_RATELIMIT_DIR, Env:CONFIRM_LIVE -ErrorAction SilentlyContinue
$S = '.\target\release\sender.exe'
Get-ChildItem Env: | Where-Object { $_.Name -match '^(SLWLAB_|RPC_|MASTER_KEYPAIR|CONFIRM_LIVE)' }
```

Expect: the last command prints nothing. (`SLWLAB_ENV_FILE` would point the binary at
`.env.devnet`; `RPC_URL` in the environment overrides `.env`: `main.rs` `Endpoints::from_env`.
`MASTER_KEYPAIR` would replace `secrets\master.json`.)
STOP: any variable is listed. Remove it with `Remove-Item Env:<NAME>` and repeat.

### A2. Platform id from `.env`

```powershell
Select-String -Path .env -Pattern '^SLWLAB_PLATFORM=' | ForEach-Object { $_.Line }
```

Expect: exactly one line, `SLWLAB_PLATFORM=<your id>` (README example `win-pc1`). Calibration
and every confirmatory session must use this one id (`host.rs` `gate_live`).
STOP: no line (calibrate refuses with "unregistered": `host::require_platform`), or an id that
differs from the one you want to keep for the whole project.

### A3. Binary hash

```powershell
(Get-FileHash $S -Algorithm SHA256).Hash.ToLower()
```

Expect: `903525409e18f5ad4aedfaffcdc2fb60e3b63ff0fc596ae834769469e4687317`
STOP: any other value. Do not rebuild in the middle of a runbook: the hash is written into
every session header and every manifest row, and Windows locks the file while a session runs.

### A4. PC checklist (all before S6)

- Screen lock with **Win+L**. Never sign out: a sign-out kills the supervisor silently
  (session `20261023T210440Z`; README "what can and cannot be caught").
- Windows Update paused (Settings > Update & Security > Pause updates). The pre-flight refuses
  a live start when `RebootRequired` or `RebootPending` exists (`preflight.rs`
  `enforce_refusals`); `PendingFileRenameOperations` alone only warns.
- All other apps closed (free RAM >= 2 GiB or a warning; 3.3 of 8 GiB free was seen on 10-25).
- At least 10 GB free on C: (`Get-PSDrive C`). The pre-flight refuses a live start under 5 GB and
  warns under 10 GB; a running session stops before its next reservation under 1 GB. Debug
  builds, `cargo test` and `clippy` never run on C: (they grow to 14 GB): set
  `$env:CARGO_TARGET_DIR = "D:\slwlab-target-debug"` first (create it) and delete that folder
  when done. Only `target\release` stays on C:; `data\`, `secrets\` and the repo stay on C:.
- Plugged in (AC). The pre-flight warns if the PC sleeps or hibernates after N s idle on AC.
- Wi-Fi: PC next to the router, TP-Link USB adapter on 2.4 GHz ch 13. The pre-flight warns
  below signal quality 60 %; power saving must stay at Maximum Performance (AC index 0).
- One console window for the session. Closing it is caught (`console_close`) and stops the
  session; a sign-out is not.

### A5. Open session left from 10-23

`data\sessions\open.json` currently holds the dry-run session `20261023T210440Z` (it died at
21:47Z). A calibrate session refuses to start while a session of another mode is open
(`session::open_or_resume`).

```powershell
Test-Path data\sessions\open.json
```

If `True`:

```powershell
& $S session --reset
```

Expect: possibly `the open session ended abnormally: last heartbeat ..., last event ...
(abnormal_end recorded)`, then `session 20261023T210440Z closed`.
STOP: `session <id> may still be running (heartbeat N s ago)`. Close the other window, wait
until the heartbeat is older than 90 s (`DOWN_AFTER_SECS`), repeat.
If `False`: skip.

---

## S1) Balances (read-only)

```powershell
& $S balances
```

Expect, on stdout, one line:
`data\snapshots\balances_<UTC>_slot<N>.json total 100000000 lamports, locked in payers+sink 0 (slot <N>)`
and on stderr an info line containing `cluster=mainnet` (`main.rs` logs `connected`).
The path has no `nonmainnet` in it (non-mainnet data goes to `data\nonmainnet\<cluster>\`).
Master = 100,000,000, payers = none yet, sink = 0.

```powershell
Get-Content (Get-ChildItem data\snapshots\balances_*.json | Sort-Object LastWriteTime | Select-Object -Last 1).FullName
```

Expect: `"cluster": "mainnet"`, master `["master", 100000000]`, sink `["sink", 0]`, no payers.
STOP: total is not 100000000; the cluster is not mainnet; an error mentioning
`secrets dir ... is bound to ...` (wrong env file or secrets dir; `secrets\CLUSTER` must read
`mainnet`); `RPC_URL is not set`.

## S2) Create the 32 payers

```powershell
& $S init-payers --count 32
```

Expect: 32 lines `payer_000 <pubkey>` ... `payer_031 <pubkey>`, then `sink <pubkey>` (the sink
already exists: `6y9XVG2L...` in the 10-25 snapshots). Existing payer files are reused, never
overwritten (`keys::init_payers`), so repeating is safe. Only public keys are printed.

```powershell
(Get-ChildItem secrets\payers\payer_*.json).Count
```

Expect: `32`.
STOP: any count other than 32; any error.

**Back up now, before S4:** copy the WHOLE `secrets\` folder (master.json, sink.json,
CLUSTER, payers\, devnet\) to a safe OFFLINE place (USB stick kept away from the PC). Not to
cloud sync, not into the repo (`/secrets/*` is git-ignored; `check_no_keys.py` fails the tree
if a key is tracked). Without the master key the 100,000,000 lamports cannot be recovered.
STOP until the copy exists.

## S3) Funding plan (dry-run, nothing sent)

```powershell
& $S fund --payer-target-lamports 813090
```

`813090` = rent minimum 650,240 + 5,000,000 / 32 (rounded up) + 6,600 worst-case fee
(README "Payer sizing").
Expect (stdout):
`33 transfers, 26669120 lamports moved (locked rent, not spend), fees 165000 lamports`
followed by 33 lines `<master> -> <payer or sink> <lamports> (fee 5000)`: 32 payers at
813090 each (26,018,880) plus the sink at 650,240, ending with
`dry-run: nothing sent (add --live with CONFIRM_LIVE=yes and --max-spend-lamports)`.
Master after funding would hold 100,000,000 - 26,669,120 - 165,000 = 73,165,880.
The rent minimum (650,240) and the 5,000-lamport fee are taken from the cluster at run time
(`getMinimumBalanceForRentExemption`, fee of a one-signature transfer): the numbers above are
**unverified** until you see them.
STOP: transfer count other than 33; a different fee per transfer; moved or fee totals that do
not match your arithmetic; `master has ... lamports, needs ...`.

## S4) Fund live

```powershell
$env:CONFIRM_LIVE = 'yes'
& $S fund --payer-target-lamports 813090 --live --max-spend-lamports 200000
Remove-Item Env:CONFIRM_LIVE
```

The cap 200,000 covers the 165,000 of fees plus retries of up to 7 more 5,000-lamport
transfers (`funding::MAX_TRANSFER_RETRIES` bounds the retries; the funding fees must also fit
the 5,000,000 reserve: `check_reserve`).
Expect, in order: `opening snapshot: data\snapshots\balances_<UTC>_slot<N>.json`, then a JSON
reconcile report, then `closing snapshot: data\snapshots\balances_<UTC>_slot<N>.json`. In the
report: `"ok": true`, `"problems": []`, `"transfers_planned": 33`, `"transfers_missing": []`,
`"ledger_fees": 165000`, `"open_reservations": 0`, `"locked_rent_after": 26669120`
(`after_total` 99,835,000). Exact field values other than `ok`/`problems`/`transfers_*` are
**unverified**; the field names are in `funding::ReconcileReport`.

Keep the closing snapshot path; every later reconcile starts from it:

```powershell
$POST = (Get-ChildItem data\snapshots\balances_*.json | Sort-Object LastWriteTime | Select-Object -Last 1).FullName
$POST
```

Expect: the path printed after `closing snapshot:`. Write it down.
STOP: the command exits with `RECONCILIATION FAILED: ...`; `transfers_missing` is not empty
(re-running `fund` with the same flags tops up only what is missing, but tell Claude first);
`live sending requires env CONFIRM_LIVE=yes`; `exceeds HARD_CEILING_LAMPORTS`.

## S5) Pre-flight without sending

There is **no dedicated pre-flight subcommand.** What exists:

1. Environment pre-flight only (power plan, RAM, network, Wi-Fi, clock, restart markers). It
   runs the ignored unit test, builds a debug binary in `D:\slwlab-target-debug` (the release
   `sender.exe` and its hash are not touched) and sends nothing but one NTP exchange:

   ```powershell
   $env:CARGO_TARGET_DIR = "D:\slwlab-target-debug"
   cargo test -p slwlab-sender --lib preflight::tests::on_this_machine -- --ignored --nocapture
   ```

   Expect: a JSON block and `WARNING: ...` lines; `"restart_pending": []`;
   `"pending_file_rename_operations"` with the Bitdefender/EdgeUpdate entries (warning only);
   `"network"."type": "wifi"` and `wifi_link` with channel 13, band `2.4 GHz`, signal about
   96; `"clock"."local_minus_server_ms"` within +-50; `test result: ok`. It uses the fixed
   platform id `check`, so the platform line is not the real one. Debug build time is
   **unverified**.
   STOP: `restart_pending` is not empty (restart Windows, check again); `local_minus_server_ms`
   beyond +-50 or no NTP answer; signal quality < 60; sleep or hibernate after N s on AC.

2. The **payer check** (every payer holds rent minimum + one worst-case fee, and the total
   spendable covers the run's budget) exists only inside `session --mode calibrate|measure`
   and `live`, and runs before the session is opened or anything is sent. S6 therefore starts
   with it. If it refuses, nothing was sent.

3. Optional, full pipeline without spending (exploratory dry-run data, costs about 147
   credits per 10 min): `& $S session --mode dryrun --hours 0.1`.

## S6) 20-minute smoke run (live, small budget)

A session, not a bare `live` run, so that `preflight.json`, `rtt.jsonl`, `wifi.jsonl` and
`network.json` are written. 0.34 h = 1224 s.

```powershell
$env:CONFIRM_LIVE = 'yes'
& $S session --mode calibrate --hours 0.34 --max-spend-lamports 300000 --offsets -300,-100,0,100
Remove-Item Env:CONFIRM_LIVE
```

The offsets are the README example; `calibrate.py` later picks the offset with the highest
usable-window rate. 300,000 is the smoke share of the 5,000,000 calibration budget (S7 gets
4,700,000, so the two together cannot exceed 5,000,000).

Expect, in order: `WARNING: ...` lines (Wi-Fi, `PendingFileRenameOperations is set (N
entries ...)`, `platform` only if unset), then
`starting calibrate session <id> until <UTC end> (sender 903525409e18)`, then the worker
runs about 20 min silently apart from log lines on stderr, then
`session <id> done; coverage by UTC hour: data\sessions\<id>\coverage.md`.
Do not touch the PC except Win+L.
STOP before anything is sent (the command exits with `error: ...`): `pre-flight refuses a live
start: Windows is waiting for a restart (...)`; `--payer-target-lamports` fix text from the
payer check (re-run S3/S4 with the number it prints); `live sending requires env
CONFIRM_LIVE=yes`; `calibration hosts` / platform / network-type refusals from `gate_live`.
STOP during the run: Ctrl+C once, then S9.

### Checks after the smoke run

```powershell
$ID = (Get-ChildItem data\sessions -Directory | Sort-Object Name | Select-Object -Last 1).Name
$ID
Get-Content data\sessions\$ID\preflight.json | ConvertFrom-Json | Select-Object -ExpandProperty info | ConvertTo-Json -Depth 6
```

Expect (`preflight.json`): `restart_pending` `[]`; `pending_file_rename_operations` listed (not
a refusal); `network.type` `wifi`; `wifi_link` band `2.4 GHz`, channel 13, signal near 96;
`clock.local_minus_server_ms` near +20 (sign as the code defines it: local minus server);
`platform` your id; `shutdown_event_deliverable` true; `ui_dlls_loaded` empty.

```powershell
Get-Content data\sessions\$ID\network.json | ConvertFrom-Json | Select-Object rtt, wifi_link_samples, network_types, bands, channels, access_point_changes, signal_quality | ConvertTo-Json -Depth 6
```

Expect (`network.json`): `rtt.n` about 230-240 (one probe per 5 s over 20 min, first probe
of each connection excluded), `rtt.n_err` 0 or a handful, `rtt.min_ms`, `rtt.max_ms`,
`rtt.jitter_ms` present; `wifi_link_samples` about 40 (one per 30 s); `network_types`
`{"wifi": N}`; `bands` `{"2.4 GHz": N}`; `channels` `{"13": N}`; `access_point_changes` 0;
`signal_quality.min` >= 60. Percentile field names inside `rtt` beyond those listed are
**unverified**.

```powershell
(Get-Content data\sessions\$ID\rtt.jsonl).Count
(Get-Content data\sessions\$ID\wifi.jsonl).Count
Select-String -Path data\sessions\$ID\events.jsonl -Pattern 'feed_stale|worker_down|abnormal_end|down|console_close'
(Get-Content data\exploratory\calibration_manifest.jsonl).Count
& $S reconcile --since $POST
```

Expect: `rtt.jsonl` about 240 lines, `wifi.jsonl` about 40; no event matches (a few `feed_stale` /
`feed_recovered` pairs of a few seconds are tolerable, tell Claude); the calibration manifest has
lines (`runner.rs` writes `data\exploratory\calibration_manifest.jsonl`); reconcile prints a
JSON report with `"ok": true`, `"open_reservations": 0`, `"problems": []` and
`ledger_fees` <= 300000 (it also saves a new snapshot).
How many windows 20 minutes yields and what they cost is **unverified** (depends on the
leader schedule); the ledger stops the run at the cap either way.
STOP: `rtt.n_err` large, `wifi_link_samples` 0 or `signal_quality.min` < 60,
`access_point_changes` > 0, `abnormal_end` or `down` in events, an empty calibration manifest,
`"ok": false`, `ledger_fees` above 300000. Do not start S7.

## S7) 3-hour calibration session

Budget split, so smoke + calibration stay within 5,000,000 in total:

| Step | `--max-spend-lamports` |
|---|---|
| S6 smoke | 300,000 |
| S7 calibration | 4,700,000 |
| Total | 5,000,000 |

The binary enforces the same ceiling a second time: the planner never plans past
`CALIBRATION_BUDGET_LAMPORTS - already committed` (`main.rs` `purpose_budget`), whatever caps you
type. Payers hold 32 x (813,090 - 650,240) = 5,211,200 spendable lamports, enough for 5,000,000.
If the smoke spent less than 300,000 the rest stays unspent; do not raise the S7 cap.

Before starting: S6 checks all passed; A4 checklist done; the session lasts 3 h of daytime.
In the same window:

```powershell
$env:CONFIRM_LIVE = 'yes'
& $S session --mode calibrate --hours 3 --max-spend-lamports 4700000 --offsets -300,-100,0,100
Remove-Item Env:CONFIRM_LIVE
```

Expect: same start lines as S6 (`starting calibrate session <id> until <UTC end> (sender
903525409e18)`), then up to 3 h running, ending with `session <id> done; coverage by UTC hour:
data\sessions\<id>\coverage.md`. At the end the supervisor runs `sender collect` itself
(`collect` event in `events.jsonl`).
The session ends at its fixed UTC end even when the budget is used up earlier; whether the
planner runs out of budget before 3 h is **unverified**. Spend stops at the cap in any case.
STOP: same refusals as S6; Ctrl+C once at any sign of trouble, then S9.

**With the pacing build (rebuilt after 2026-10-25):** the spend follows a straight line over the
session, so S7 spends about 4,700,000 / 3 = 1,567,000 lamports per UTC hour (roughly 50 windows an
hour) instead of finishing in the first 25 minutes, and the smoke run spreads its 300,000 over its
20 minutes (about 9 windows, one every 2 minutes). Check `coverage.md` after each session: the
`reserved lamports` column should be about the same in every full hour (within ±15 %), the
`Windows skipped, by reason` table shows `pacing: # + # > # (plus one window)` as the big count, and the
total stays at or below the cap. STOP: one hour holds more than 1.5x the average, or the
reserved total is above `--max-spend-lamports`. If a worker ends normally with less than one
window's worst-case cost left (39,600), the session ends as done (event `cap_reached`).

## S8) Afterwards

All commands from the repo root. `$POST` is the closing snapshot path from S4 (set it again
if the window was closed: `$POST = 'data\snapshots\balances_<...>.json'`).

1. Resolve anything still open (idempotent; a session already did it once):

   ```powershell
   & $S collect
   ```

   Expect: `... to resolve, N manifest lines written, 0 still pending` style lines (exact
   wording in `main.rs` `collect_all`) and exit 0.
   STOP: a non-zero `still pending` count after two runs.

2. Reconcile wallets, ledger and on-chain fees since funding:

   ```powershell
   & $S reconcile --since $POST
   ```

   Expect: `"ok": true`, `"problems": []`, `"open_reservations": 0`, and `wallet_delta`
   equal to `ledger_fees` (`wallet_delta` is before-total minus after-total, so it is
   positive when fees were paid; seen on 2026-10-25: both 254,448 after the smoke and both
   4,843,462 after the calibration). `ledger_fees` for the whole calibration (smoke + S7)
   <= 5,000,000.
   STOP: `RECONCILIATION FAILED: ...`; fees above 5,000,000.

3. Independent verification, offline:

   ```powershell
   .\.venv\Scripts\python.exe scripts\verify_manifest.py
   ```

   Expect: `txs N, landed N, saved blocks N`, per-check lines `M: n/n ok` ... `Q: n/n ok`,
   and `OK -> data\verify\verify_<UTC>.json`, exit 0. Check Q needs every calibration row to
   carry your registered platform and one network type (`wifi`).
   STOP: any `FAIL` line or `N PROBLEMS`.
   Optional online re-fetch of 20 saved blocks (read-only; the key is put in this window's
   environment and removed again, never printed):

   ```powershell
   $env:RPC_URL = ((Select-String -Path .env -Pattern '^RPC_URL=').Line -replace '^RPC_URL=','').Trim('"')
   .\.venv\Scripts\python.exe scripts\verify_manifest.py --online --sample 20
   Remove-Item Env:RPC_URL
   ```

   Credits used by this are **unverified**; STOP on any `FAIL`.

4. Estimate the nuisance parameters (order-agnostic, EXPLORATORY):

   ```powershell
   cd analysis
   ..\.venv\Scripts\python.exe -m slwlab.calibrate --data ..\data --out sim\results\nuisance_calibrated.json
   cd ..
   ```

   Expect: one JSON line mapping p_land, clock_sd_ms, offset_ms, bg_between ... to values, and
   the file `analysis\sim\results\nuisance_calibrated.json` (inside it `deviation: true`
   marks a value outside the preset low-high range; that is not a failure: REPORT.md must carry
   it as a labeled deviation, PREREGISTRATION §0.4).
   STOP: a traceback; an estimate that is `null` (too little data, tell Claude).

5. Check the tree has no secrets, add a LOGBOOK entry (UTC times, from `date -u`; Claude
   writes it with you), regenerate and test, then commit locally:

   ```powershell
   .\.venv\Scripts\python.exe scripts\check_no_keys.py
   cd analysis
   ..\.venv\Scripts\python.exe -m slwlab.report
   ..\.venv\Scripts\python.exe -m slwlab.report --check
   cd ..
   git status --short
   ```

   Expect: `check_no_keys.py` exits 0 and prints no finding; `report --check` says docs/REPORT.md
   matches its inputs. `git status` lists only `data\` (sessions, logs, snapshots, ledger,
   exploratory, verify, raw), `analysis\sim\results\nuisance_calibrated.json`, `docs\`.
   STOP: `secrets\` anywhere in `git status`; a key or `api-key=` finding; `report --check`
   fails. Then:

   ```powershell
   git add data analysis/sim/results/nuisance_calibrated.json docs
   git commit -m "Calibration data: mainnet smoke + 3 h session, reconcile and verify OK"
   ```

   Do NOT push (no push until told). `prereg-v1` is tagged later, after calibration and the
   re-derived numbers (PREREGISTRATION §0.5), not here.

## S9) If something goes wrong

A live session is **never resumed.** The ledger is fsync'd before every send, so nothing is
lost or double-spent. Recovery is always: reset, collect, reconcile, then a new session.

**Common recovery (after the session died, the PC restarted, or you stopped it):**

```powershell
& $S session --reset
& $S collect
& $S reconcile --since $POST
```

Expect: `--reset` prints `session <id> closed` (after an `abnormal_end` line if the supervisor
died); `collect` ends with nothing pending; reconcile `"ok": true`.
STOP: `session <id> may still be running (heartbeat N s ago)` (wait 90 s, or close the other
window); reconcile `"ok": false`.
Then start a new `session --mode calibrate` (S7 command). The new session continues the
allocation from the ledger (`runner.rs` `prior_plan_state`): no window attempted before is
planned again. Set `--max-spend-lamports` to the amount you still want to allow (5,000,000
minus everything calibration has reserved so far, from `ledger_fees` of the reconcile); the
binary also caps it at the purpose budget. A partial session counts as done work: the 3 h are
not required to be contiguous, but `coverage.md` shows the gap by UTC hour.

**The session dies (crash, power loss, sign-out, forced kill):** a sign-out cannot be caught;
the supervisor and worker just vanish. A worker crash alone is restarted by the supervisor
after 30 s (`worker_down`) with `collect` first. After a supervisor death, do the common
recovery. The next start first appends `abnormal_end` (last heartbeat, last event).

**The PC restarts:** same as above. After the restart: sign in, run A1-A3 again (new window,
env clean), check `RebootRequired`/`RebootPending` are False (a restart that Windows still
wants is a refusal), then the common recovery. Windows Update stays paused.

**The session ends FAILED (`crash_limit` in `events.jsonl`):** the worker crashed a sixth time within
an hour. Read the last `run.log.jsonl` under `data\logs\live-*`, fix the cause, then
`sender session --reset`, `sender collect`, `sender reconcile --since <snapshot>` and start a new session.

**A worker exits with code 3 (`worker_refused` in `events.jsonl`):** fewer than 12 payers hold the
rent minimum plus one worst-case fee, or the usable payers cannot spend the budget. The supervisor
does not restart it (no loop) and the session ends as done. Run the `sender fund` command printed in
the error (dry-run first), then start a new session. Payers a few hundred lamports under the floor
no longer stop a session: they are listed as a WARNING and skipped.

**You stop it with a single Ctrl+C:** the worker stops planning, drops windows not yet started,
writes its `summary.json` (live: after a bounded collect pass) and the supervisor prints
`live session <id> stopped. It will not resume: sender session --reset, then a new session
continues the allocation from the ledger`. Then the common recovery. A second Ctrl+C exits at
once; `collect` resolves what remains.

**Console window closed or shutdown:** caught (`console_close` / `shutdown` events), same as
Ctrl+C; Windows ends the process about 5 s later.

**Abort the whole attempt:** `& $S sweep` (dry-run plan), then
`$env:CONFIRM_LIVE='yes'; & $S sweep --live --max-spend-lamports 200000; Remove-Item Env:CONFIRM_LIVE`
returns payer and sink balances to the master (README). Everything in `sweep` is
**unverified on mainnet**; it was exercised on devnet only (66 transfers landed 10-25).
