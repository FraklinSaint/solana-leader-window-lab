# solana-leader-window-lab

**Inside the Block: How Solana Validators Order Transactions** (Superteam Ukraine bounty; full text
in [docs/BOUNTY.md](docs/BOUNTY.md)).

We send small, controlled bursts of our own transactions into the leader windows of chosen
mainnet validators. From the produced blocks we then decide which candidate ordering rule each
validator follows, how strictly it follows it, or whether it follows any rule at all.

> **Status: Phase 2 built; Phase 1 (rules, statistics, simulator, power analysis, analysis
> pipeline) built; the pre-registered *procedure* is drafted. Its numbers are provisional
> (pre-calibration SIMULATION) and are re-derived after calibration, then frozen.** No
> transaction has been sent to mainnet or devnet. No measurement exists yet. All dry-run data is
> exploratory. Phase 3 (a devnet run that funds, sends, collects and sweeps) has **not** been
> done; on devnet only a read-only selection and a 2-minute dry-run exist (see "Phases").

## Quick start

```bash
cp .env.example .env              # set RPC_URL and RPC_WS_URL (env only; see "RPC provider")
docker compose up                 # select validators (if needed) + print the window plan. Never sends.
docker compose run --rm sender dry-run --duration-secs 86400   # real-time loop, nothing sent
# Windows native, one daytime session (see "Running natively"):
#   cargo build --release --locked -p slwlab-sender
#   .\target\release\sender.exe session --mode dryrun --hours 8
docker compose run --rm analyze   # regenerate docs/REPORT.md from committed data/
docker compose run --rm verify    # re-fetch every block and check it against data/manifest.jsonl
# live (operator only; the cap is mandatory and cannot exceed 0.05 SOL):
docker compose run --rm keys init-payers --count 32
docker compose run --rm sender fund --payer-target-lamports 813090            # prints the plan (sizing: "Payer sizing")
docker compose run --rm -e CONFIRM_LIVE=yes sender fund --payer-target-lamports 813090 --live --max-spend-lamports 200000
docker compose run --rm -e CONFIRM_LIVE=yes sender live --purpose calibrate --offsets -300,-100,0,100 --max-spend-lamports N
docker compose run --rm -e CONFIRM_LIVE=yes sender live --purpose measure --prereg <commit> --allocation analysis/sim/results/alloc_v2.json --max-spend-lamports N
docker compose run --rm sender collect        # resolve anything left open (crash recovery)
docker compose run --rm -e CONFIRM_LIVE=yes sender sweep --live --max-spend-lamports 200000
docker compose run --rm sender reconcile --since data/snapshots/balances_<first>.json
```

`sender` subcommands: `session`, `select-validators`, `leader-snapshots`, `plan`, `dry-run [--probe-connect]`,
`live`, `collect`, `init-payers`, `balances`, `fund`, `sweep`, `reconcile`, `clock-report`. Run `sender <cmd> --help` for options.
Data from any cluster other than mainnet (decided by genesis hash) goes to
`data/nonmainnet/<cluster>/` and is never analyzed.

## RPC provider

Provider: **Helius, free plan** (1M credits/month, 10 req/s, `sendTransaction` 1/s, 5 WebSocket
connections; docs checked 2026-10-22). Endpoints come **only** from the environment
(`RPC_URL`, `RPC_WS_URL`, or a git-ignored `.env`). There is no default endpoint, no CLI flag
for it, and no provider region or key anywhere in the code.

- **Rate limit.** One budget for the whole machine: every `sender` process (live, dry-run,
  collect, select-validators, ...) and `scripts/verify_facts.py` share a GCRA token bucket
  stored in `data/.ratelimit/` under an OS file lock (`ratelimit.rs`, Python twin
  `scripts/ratelimit.py`). It allows 8 req/s sustained, burst 2, so at most 10 in any second
  across all processes. Retries count too. `sendTransaction` additionally waits for its own
  shared 1/s bucket. A 429 is retried with exponential backoff (250 ms doubling,
  8 s cap, `Retry-After` honoured) and logged. `--rpc-rps` / `--send-tx-rps` can only lower
  the limits.
- **WebSockets.** At most 4 per process. Only standard `slotSubscribe` is used:
  `slotsUpdatesSubscribe` is not offered on Helius LaserStream WSS.
- **Standard methods only.** `rpc.rs::ALLOWED_METHODS` refuses anything else before a request
  is made. There is no transactionSubscribe, no blockSubscribe and no paid-only extension.
  Outcomes come from `getBlock` and `getSignatureStatuses`.
- **Credits.** Every call and every received WebSocket byte is metered (1 credit per call;
  2 credits per 0.1 MB streamed). Each run's `summary.json` has the per-method totals, and every
  command appends to `data/credits/credits.jsonl`. A warning fires when the month-to-date
  estimate reaches 50% of the quota. The Helius dashboard is the authority; this is an
  estimate.
- **API-key redaction.** `api-key=` values are replaced by `REDACTED` in every log writer,
  error message and stored endpoint (`redact.rs`). `cargo test --test no_api_keys` and
  `make secrets-check` fail if a key pattern appears in any tracked file or anywhere under
  `data/`.
- **RPC delivery check = `rpc-helius-staked`.** Every k-th live window (`--rpc-every k`) sends
  **exactly one tx** (the first in send order) through Helius `sendTransaction`. It is never
  a burst arm: the free plan allows 1 sendTransaction/s. Its manifest lines carry
  `ordering_eligible: false` and it is reported separately. No ordering conclusion is drawn
  from it. The question it answers: does a (documented) staked path land where our unstaked
  QUIC does not? Helius documents staked routing as the default ("Staked Connections
  (default)", 1 credit), but states it explicitly only for paid plans. If the free plan rejects
  or does not land these txs, that is a reported landing result, and the path is dropped.
  Helius Sender is not used: it requires tips.

## Phases

Order (Phase 0 gate, corrected at the Phase 1 gate): **2** (sender, ledger, dry-run, clock) →
3 (devnet end-to-end with sending, collect and sweep, operator; **not done yet**) → 4 (mainnet dry-run in daytime session chunks, merged by
UTC hour, selection snapshot, clock error report) → 5 (calibration, ≤ 5,000,000 lamports, operator, **exploratory**) → **re-run the
simulator with nuisance parameters estimated from calibration** (`slwlab.calibrate`,
order-agnostic statistics only), re-derive ceilings, thresholds, allocation and the positive
control interval, then **freeze and tag `prereg-v1`** → 6 (confirmatory live campaigns on
≥ 3 dates, ≤ 38,000,000 lamports) → 7 (report). The pre-registered object is the
*procedure* (docs/PREREGISTRATION.md §0). Its numbers are filled in after calibration and
before the first confirmatory window. Phase 1 (parser, rules, stats, simulator) is built.

**What exists on devnet** (`data/nonmainnet/devnet/`, 12 files, committed in `8abd145`, never
analyzed). On 2026-10-22, on the dev PC with `SLWLAB_ENV_FILE=.env.devnet` (Helius devnet
endpoint), two read-only commands were run. Nothing was funded or sent:
- `sender select-validators` at 19:15:00Z: 4 snapshots (`cluster_nodes_`, `vote_accounts_`,
  `kobe_validators_` and `selection_20261022T191500Z`). Devnet has few labeled clients: 5
  validators were picked, and JitoAgave got 1 of the 2 required;
- `sender dry-run --duration-secs 120` at 19:15:11Z: `logs/dryrun-20261022T191511Z/`
  (`run.json`, `plan.jsonl`, `windows.jsonl`, `clock.jsonl`, `slots.jsonl`, `run.log.jsonl`,
  `summary.json`, and `clock_hourly.json`, which the run writes itself). It planned and executed
  18 windows with 32 ephemeral payers and **sent 0 transactions** (`txs_sent: 0`); window-start
  error median 56 ms, p95 432 ms.

It checked cluster detection by genesis hash, the routing of non-mainnet data to
`data/nonmainnet/`, selection and planning on a second cluster, and the slot feed. It did not
check `init-payers`, airdrop, `fund`, `live`, `collect`, `sweep` or `reconcile`. Those are Phase 3,
still open. The localnet end-to-end test (`localnet_tests.rs`, CI job `localnet`) covers them on
a local validator but has not run yet: it runs only in CI, which has not run yet.

Without Docker: `make test`, `make facts`, `make verify`, `make verify-online`.

## Sessions on one Windows PC

Every run happens on one Windows PC that cannot stay on overnight: **3–4 sessions of 6–10 h
in the daytime, on different dates**. A VPS was planned and dropped (identity verification
blocked it), and no VPS code remains in the repo.

**Plan.** `make sessions` (`analysis/slwlab/sessions.py`) writes `docs/SESSIONS.md` (PLANNING,
not measurement) from the saved leader schedules. It reports windows per hour and the hours
each validator and each (validator × arm) cell needs for its quota, then replays session
scenarios (3–4 × 6–10 h, quota split evenly over the sessions so that every cell has windows
from every session's UTC hours) and reports, per cell, the attempted and expected usable
windows, the power (SIMULATION curves) and the cost. The binding stratum is vanilla Agave:
`5Us18hLZ` has ≈ 11 windows/h and needs ≈ 15 h for its 166 attempted windows. Two designs are
shown side by side. **The choice is the operator's, and it is pre-registered before any
confirmatory data:**
- **A. Per-validator quotas** (current design). Each chosen validator is its own cell, which
  matches the bounty's "for each validator you pick". It fits 3 × 8 h at 15% lost windows. At
  30% loss, or with 3 × 6 h sessions, the slow Agave validator falls short (e.g. C 14/17 attempted
  at 3 × 6 h).
- **B. Vanilla Agave pooled at stratum level** over the 2 chosen + 4 more 4.3.2 validators
  with the same total quota. It needs fewer hours (≈ 12 vs ≈ 19 continuous h at 15% loss), but
  the 4 added validators contribute only ≈ 20% of the windows (≤ 6 windows/h together). The
  claim becomes stratum-level, and its power holds only if every pooled validator follows the
  same rule. Per-validator statements for `5Us18hLZ` get fewer windows than in A.

### Running natively

The entry point is the `sender` binary itself; there is no PowerShell or batch runner, and
none may be added beyond a one-line call of the binary (`analysis/tests/test_script_hygiene.py`
fails on in-memory interop, encoded commands, expression evaluation and hidden windows in any
tracked script). A PowerShell runner with that kind of interop was removed from the operator's
disk on 2026-10-23, most likely by antivirus (LOGBOOK).

```powershell
cargo build --release --locked -p slwlab-sender           # once, and after every code change
.\target\release\sender.exe session --mode dryrun --hours 8   # in a normal PowerShell, repo root
```

`sender session --mode dryrun|calibrate|measure --hours <= 10`:
- **pre-flight** (`crates/sender/src/preflight.rs`), recorded in `data/sessions/<id>/preflight.json`
  and printed as warnings: sleep and hibernate timeouts (`powercfg`), free RAM, the
  default-route adapter and its type (**Wi-Fi**, Ethernet, VPN; on Wi-Fi band, channel, signal
  quality, RSSI, PHY, power-saving mode: warnings only, never a refusal), the clock offset against an NTP server
  (SNTP, `--ntp-server`, default time.cloudflare.com), the platform id (`SLWLAB_PLATFORM`),
  whether Windows Update or servicing is waiting for a restart (registry, `reg query`), and the
  free space of the data volume: a live start is refused under 5 GB, warned about under 10 GB,
  and a running live session stops before its next reservation under 1 GB (fail closed: the
  ledger line is written before the first byte of a window is sent, and a ledger write that
  fails aborts the run before any send);
- **session header** `data/sessions/<id>/session.json`: mode, fixed UTC interval, platform,
  network type, OS build, machine id, sender SHA-256;
- **network path record** (Wi-Fi limitation): `rtt.jsonl` (RTT to the RPC endpoint every 5 s),
  `wifi.jsonl` (link state every 30 s) and, at the end, `network.json` and a section of
  `coverage.md`;
- **no idle sleep**: `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)` through
  `crates/winsys`, the only crate with `unsafe` code (four Windows API calls behind safe
  functions; the workspace forbids `unsafe` everywhere else), held by the supervisor for the
  whole session and logged as a `keep_awake` event. It blocks **idle** sleep only. A sleep,
  sign-out or shutdown chosen by the user or by Windows still happens;
- **supervised worker**: the binary runs itself as `sender dry-run` (or `sender live` with every
  live guard: `CONFIRM_LIVE=yes`, `--max-spend-lamports`, prereg, platform gate) for the time
  left, and restarts it 30 s after a crash, at most 5 times within an hour (a sixth crash ends
  the session as failed, event `crash_limit`). A live worker is followed by `sender collect`
  before the restart, and each live worker may reserve only the session cap minus everything
  reserved since the session started;
- **dead connection check** (live, QUIC): each window opens its connection at prep time; just
  before the reservations the runner looks whether the peer has dropped it. If so it reconnects once
  when at least 400 ms remain before the burst, otherwise the window is given up with no
  reservation and no fee (`summary.json`: `conn_dead_reconnected`, `conn_dead_skipped`; session
  event `conn_dead`). Burst timing, send order and everything after the reservations are unchanged;
- **spend pacing** (live sessions): cumulative reserved spend may not run ahead of the straight
  line from 0 at the session's start to its `--max-spend-lamports` at the session's end, plus one
  window (`planner.rs` `Pace`; skip reason `pacing`). The line comes from the session (fixed start,
  end and cap, handed to every worker as `--pace-*`) and the ledger (lamports reserved since the
  session start), never from a worker's own start, so a restarted worker continues the same curve.
  Without it the whole budget is spent in the first minutes (calibration 2026-10-25: 4.7M in 23
  minutes of a 3 h session). When a worker ends normally and less than the worst-case cost of one
  window is left, the session ends as done (event `cap_reached`);
- **gaps with UTC timestamps**, never filled (`data/sessions/<id>/events.jsonl`): `down` (the
  supervisor was not running: crash, power loss, reboot), `worker_down` (restart pause),
  `stopped` (Ctrl+C, console close, shutdown), `console_close` / `shutdown` (written the moment
  Windows announces it), `abnormal_end` (see below), `feed_stale` / `feed_recovered` (the
  worker's staleness gate: no window is prepared or sent while the newest WebSocket slot
  notification is older than 2 s), plus the worker's own `gaps.jsonl` (host_stall, slot_feed,
  rpc_error);
- **what can and cannot be caught** (Windows, console app):
  - console window closed (`CTRL_CLOSE_EVENT`): caught. Windows ends the process about 5 s
    later. The supervisor logs `console_close` at once, the worker stops without its usual
    waits and writes `summary.json` (`stopped_early: console closed or system shutdown`), and
    the supervisor logs `stopped` if it is still alive;
  - system shutdown (`CTRL_SHUTDOWN_EVENT`): handled the same way, but Windows delivers it to a
    console process only if user32.dll and gdi32.dll are not loaded. `sender.exe` imports
    neither (checked in its PE import table); pre-flight records `ui_dlls_loaded` at run time
    and warns if either is present;
  - **sign-out cannot be caught**: Windows sends `CTRL_LOGOFF_EVENT` to services only and ends
    interactive processes without notice. That ended session `20261023T210440Z` (sign-out, then
    sleep). Stay signed in during a session; lock the screen (Win+L) instead. Power loss and a
    forced kill cannot be caught either;
  - every end that could not be logged is recorded afterwards: the next `sender session` (or
    `--reset`) first appends one `abnormal_end` event (last heartbeat in UTC, last event) to an
    open session whose heartbeat is stale and whose last event is not `stopped`. A start is
    refused while the open session's heartbeat is fresh (< 90 s): a second supervisor would
    start a second worker;
- **`--reset`** closes the open session: it appends `reset` (after `abnormal_end` if the
  supervisor died), moves `open.json` to `<id>/state.json` and deletes the git-ignored
  heartbeat file. It does not delete or change anything else: `session.json`, `preflight*.json`,
  `events.jsonl` (append only) and the run directories under `logs/` stay as they are. No
  `coverage.json` is written for a reset session (only a session that reaches its end gets one);
- **resume**: after a crash or reboot, running the same command continues an interrupted
  **dry-run** session until its fixed end, with a `down` gap from the last heartbeat. Nothing
  starts by itself at logon. A **live** session is never resumed: `sender session --reset`
  closes it, and a new session continues the allocation from the ledger
  (`runner.rs::prior_plan_state`), so no attempted window is planned again and no quota is
  refilled;
- **Ctrl+C** once: the worker stops planning, drops windows not yet started and writes its
  `summary.json` (live: after the bounded collect pass); the session logs `stopped`. Twice: it
  exits at once (the ledger is durable before every send; `sender collect` resolves the rest);
- **CU and loaded-accounts data**: every run simulates the binding tx of each arm at least once
  (`simulateTransaction`, `sigVerify: false`) and reports `simulation_by_arm` in `summary.json`.
  Unfunded dry-run payers give `AccountNotFound`, so the real numbers come from funded (devnet,
  calibration) runs; a live run stops if usage reaches the CU or the 32 KiB loaded-data limit;
- **coverage by UTC hour** at the end (`data/sessions/<id>/coverage.{json,md}`): seconds with a
  running worker minus its gaps, gap seconds by kind, windows, lamports reserved in the ledger
  and clock error per hour, and the skip counts per reason of the session's runs; hours with no
  coverage are flagged GAP.

The binary is the same file for the whole session (Windows keeps a running executable locked,
so a rebuild fails until the session ends), and its hash is in the session header and in every
run header.

**Merging chunks.** `make merge SINCE=<stamp>` merges all dry-run and live runs since the stamp,
by (mode, platform, RPC host) and pooled: per absolute UTC hour and per hour of day (00–23), with
`GAP` (no coverage) and `THIN` (< 30 min) flags. Hours without data stay empty. Nothing is
interpolated or borrowed from another hour or platform.

**Crash recovery, tested** (`crates/sender/tests/crash_recovery.rs`, no cluster needed). A
child process runs the runner's durable write order (plan.jsonl, fsync'd reservation per tx,
send, sent.jsonl) and is killed abruptly at random points, including inside a window. A power
loss is simulated on top with torn last lines in the ledger, plan.jsonl and sent.jsonl. After
recovery, every reservation is intact. No tx appears as sent without a reservation. The
project ceiling counts the open reservations, and a reserved signature cannot be reserved again
(no double spend). Payers of unresolved txs stay busy. The allocation continues exactly where
it stopped. After the reservations are settled, `reconcile` balances wallets against the ledger
and catches a one-lamport discrepancy. The session supervisor (`session.rs` tests, fake worker
and clock) is tested for: worker crash and restart pause, resume after a simulated power loss
with the `down` gap dated from the heartbeat, the same fixed interval after resume, coverage by
UTC hour with a GAP hour, live sessions that never resume, collect after a live crash, the
session cap, and Ctrl+C. Not tested here: a live run against a real cluster (localnet does not start on Windows; CI only).

### Payer sizing

A payer must keep the rent-exempt minimum (650,240 lamports) and can spend only what is above
it. A target of 700,000 leaves ≈ 49,760 per payer: ≈ 9 transactions, so 32 payers carry about
48 windows, nowhere near a budget of millions. So `live` and `session --mode calibrate|measure`
check **before they start**, and refuse to start if either fails:

1. at least 12 payers hold rent minimum + the worst-case fee of one transaction (a window needs
   6 distinct payers, and payers with unresolved txs are busy). Poorer payers are reported as a
   warning and skipped by the payer pool, not refused;
2. the lamports those payers can spend (Σ balance − rent minimum) are at least the run's planned
   spend: its budget, which is the purpose's remaining budget capped by `--max-spend-lamports`.

A refusal exits with code 3 (`session::EXIT_FINAL`): a supervisor never restarts a worker that
ended that way (event `worker_refused`).

The error prints the command that fixes it, `sender fund --payer-target-lamports N`, with

`N = rent minimum + ceil(budget / payers) + worst-case fee of one tx`

(the last term is slack so that the final transaction of each payer still fits). `fund` tops
every payer up **to** `N`, so it is run again before each session, and payers that kept an
unspent balance need less. Worst-case fee today: 5,000 + 1,600 = 6,600 (arm R at 200,000
µlamports/CU × 8,000 CU); rent minimum for 0 bytes: 650,240. With 32 payers:

| Run | Budget | `--payer-target-lamports` | Locked in payers |
|---|---|---|---|
| Calibration (whole phase, ≤ 5,000,000) | 5,000,000 | **813,090** | 26,018,880 |
| Confirmatory, 3 sessions (38,000,000 / 3) | 12,666,667 | **1,052,674** | 33,685,568 |
| Confirmatory, 4 sessions (38,000,000 / 4) | 9,500,000 | **953,715** | 30,518,880 |

Locked lamports are rent-exempt balance, not spend: `sweep` returns them to the master, so the
master must hold ≈ 32 × N + the sink's rent minimum + the funding fees (33 × 5,000) while the
payers are funded. If a session is started with a smaller `--max-spend-lamports` the target
shrinks with it. The pre-flight prints the exact number, so use that rather than this table when
the budget differs.

## Verify (no trust in our collector)

`scripts/verify_manifest.py` (stdlib only, read-only) re-checks every claim the analysis uses.
Offline, from the committed `data/` alone:

| Check | What must hold |
|---|---|
| M | unique signatures, `cluster = mainnet`, every slot ≥ 459,653,000 |
| B | each landed tx sits at its claimed `block_index` of the saved block; tx count, fee, error and producer (single Fee reward) match |
| T | the on-chain tx matches the manifest: fee payer, recent blockhash, signature count, CU limit, CU price and loaded-data limit decoded from its ComputeBudget instructions; only the payer is writable (plus the shared account in Arm C) |
| P | each window is in its run's `plan.jsonl`, written before sending, with the same leader, slots, arm, and per-tx limit, price and send rank |
| E | the leader-schedule snapshot of the window's epoch (`data/snapshots/leader_schedule_epoch<E>.json.gz`, the raw `getLeaderSchedule` result saved by `select-validators` and by the runner the first time it plans in an epoch, ≈ 1.1 MB per epoch) names the window's validator for all its slots and the landed slot |
| X | `exclusion_reason` re-derived from the block (producer, window bounds) |
| H | no tx marked "not landed" appears in any saved block, and no tx paid by one of our payers appears without a manifest line (no hidden or dropped data) |
| L | `data/ledger.jsonl`: every measure/calibrate reservation (= every tx sent) has a manifest line and vice versa; landed slot and fee agree |
| Q | every calibration and confirmatory tx carries a host record and all of them come from one registered machine (platform id, OS, machine id); OS builds and sender hashes are listed |

Online (`--online`, any mainnet `RPC_URL`): **R** each saved block is re-fetched and must have
the same blockhash, parent and signature order (`--sample K` for a seeded subset); **S** the
scheduled leader (`getSlotLeaders`) is the block's producer; **G** landed signatures are
finalized in the claimed slot, and "not landed" signatures are unknown to the cluster. RPC
nodes serve leader schedules only for about the last 10 epochs (≈ 10 days; epoch 1053 was
already unavailable on 2026-10-22). Later, S is reported as unavailable, and the offline
snapshot check E carries the leader evidence. The report goes to `data/verify/verify_<UTC>.json`, and the exit code is 1 on any
problem. Tested on synthetic tampering (moved position, changed price, swapped send order,
dropped tx, hidden landing, wrong exclusion, extra writable account) and, for R/S/G, against
one live mainnet block.

## Report (generated, never hand-edited)

`docker compose run --rm analyze` (or `make report`) writes `docs/REPORT.md` from committed files
only: `data/manifest.jsonl` (MEASUREMENT), `data/exploratory/calibration_manifest.jsonl`
(EXPLORATORY), `analysis/sim/results/` (SIMULATION) and `docs/LOGBOOK.md` ("What went wrong").
- **Deterministic.** There are no wall-clock times, the analysis seed is fixed (20261022), and
  tables are sorted. Inputs are listed with the SHA-256 of their LF-normalized content, so
  Windows and Linux checkouts give the same bytes. The last line is the SHA-256 of the report
  body. `make report-check` (run in CI) regenerates the report and fails if the committed file
  differs.
- **Labels never mix.** Measured verdicts and the SIMULATION reference (ceilings, allocation,
  power) are separate sections. Calibration is EXPLORATORY and never yields a verdict.
- **Scope.** `data/nonmainnet/` is never opened. Lines from another cluster or before slot
  459,653,000 are dropped, and the drops are counted in the report.
- **Dates.** Every window is listed with its slot and its block time in UTC. Windows that
  never landed show their run's UTC start. Each validator × arm row lists the UTC dates of its
  usable windows.
- With no measurements, every measured section says **NO DATA YET**.

## Safety invariants

- **Dry-run by default.** Live mode requires `live`, `CONFIRM_LIVE=yes` and an explicit
  `--max-spend-lamports N`, with `N ≤ HARD_CEILING_LAMPORTS = 50_000_000` (a compile-time constant).
- **Alpenglow guard.** The code refuses any slot `< 459_653_000` (see VERIFIED_FACTS F1). Live
  plans additionally require `epoch ≥ 1065`.
- **Write-ahead spend ledger** (`data/ledger.jsonl`). Every tx is reserved at its worst-case fee,
  and the reservation is fsync'ed *before* the first byte is sent. A reservation is released only
  on proof: the tx landed (actual `meta.fee`), or it is past `lastValidBlockHeight` at
  *finalized* commitment and absent from history. `reconcile` checks wallet deltas, ledger and
  on-chain fees against each other, and fails loudly on any mismatch.
- **One writer.** The ledger holds an exclusive OS lock (`ledger.jsonl.lock`) for its whole
  lifetime. A second `live`, `fund`, `sweep` or `collect` refuses to start while one runs, and
  the OS releases the lock if the holder crashes.
- **Keys** come only from `./secrets` (gitignored, mounted read-only except in the `keys`
  service) or env. Secret bytes are never logged. CI runs gitleaks and a keypair-pattern check.
- **One cluster per secrets dir** (`keys.rs::guard_secrets_cluster`). Every command checks it
  right after reading the genesis hash, before any key is read. A secrets dir is bound to one
  cluster by a `CLUSTER` file (written by `init-payers`). A dir bound to another cluster is
  refused: devnet keys never run against mainnet, and the reverse. An unbound dir with keys is
  accepted only if its folder is named after this cluster (`secrets/devnet`, then bound).
  Otherwise the operator has to write the marker by hand.
- **Budgets by purpose** (`crates/common/src/limits.rs`): calibration ≤ 5M, confirmatory ≤ 38M,
  funding/sweep/retry reserve ≤ 5M, total ≤ 48M lamports, under the 50M compile-time hard
  ceiling. The planner plans only within its purpose's remaining budget, and fund/sweep refuse
  to run past the reserve.
- **No Jito bundles, tips or block-engine endpoints.** Plain transactions only.
- **Provider API key** only in env / `.env` (git-ignored). It is redacted everywhere, and a
  test fails on any leak (see "RPC provider").

## Why this design

_To be written in Phases 1–4._ Summary of the logic:
- **Unit of replication:** one 4-slot leader window.
- **Primary evidence:** only our own txs.
- **Four arms** (P, F, X, R). They make the candidate rules predict *different* permutations, so the
  data can discriminate between rules.
- **Statistics:** pre-registered.
- **Power and false-positive rate:** measured on a simulator before any live data.

## Bounty considerations (one subsection each)

### Leader schedule and leader types
A leader window is a run of consecutive slots led by one validator: 4 slots, or 8+ when two
windows of the same validator are adjacent (VERIFIED_FACTS F3). The planner reads
`getSlotLeaders` in overlapping chunks and plans each window at most once. `select-validators`
labels every voting validator from gossip `clientId`, version scheme and Jito Kobe, with an
evidence list and a confidence level (F7). It then picks per stratum: vanilla Agave, Jito-Agave
and Firedancer are required; Harmonic (Agave/Firedancer), BAM and Rakurai are upside.
Conclusions are per validator and date, never per client.

### Path from our backend to the executing node, and everything in between
Primary path: direct QUIC to the leader's gossip `tpuQuic` (ALPN `solana-tpu`, one
unidirectional stream per tx). The connection is opened about 1.5 s before the burst; all txs are
signed in advance, and the burst loop only writes bytes, each within 2 ms of its planned
offset. Delivery check: `live --rpc-every k` sends one tx of every k-th window through RPC
`sendTransaction`, recorded as path `rpc-helius-staked` and excluded from ordering analysis
(see "RPC provider"). `summary.json` reports sent/errors/landed/expired per path. Each tx records monotonic and wall-clock send times, connection and stream
id, handshake time and RTT. Send time comes from a slot clock fitted to `slotSubscribe`
notifications. Its slope uses ~600 observations, its intercept is re-anchored on the last 3
(`--clock-window`, `--clock-anchor-k`, recorded in run.json), and it is re-predicted
continuously while waiting. `examples/clock_eval.rs` replays a recorded feed to compare
parameters (exploratory). Every window logs predicted vs observed start
(`clock.jsonl`). Caveats: we are an unstaked client (QoS limits, F9). For BAM validators the TPU
address may route to a BAM node (F7). Harmonic FIFO timestamps arrival "at the builder". The
public RPC slot feed was too noisy for sub-window timing (median 217 ms, p95 1.3 s). With the
Helius feed, a 10-min dry-run gave median 75 ms, p95 200 ms (exploratory, LOGBOOK).
**Limitation of this metric:** "prediction error" is measured against the moment the window's
first slot *notification arrives at our machine*, not the leader's real block start. The two
differ by the provider's propagation delay, which is unknown, possibly variable, and absorbed
by the calibrated send offset. Real window-hit accuracy is judged in calibration by the slot
in which our txs actually land (and by `producer_mismatch` exclusions).
**Platform.** The sending machine is part of this path, so **calibration and every
confirmatory window run on one machine, the operator's Windows PC**. Every run header
(`run.json` `host`) and every manifest row (`tx.host`) record the platform id
(`SLWLAB_PLATFORM` in `.env`), OS, OS build, a machine id (first 16 hex of SHA-256 of the Windows
MachineGuid) and the SHA-256 of the sender binary that ran (`host.rs`). `live --purpose
calibrate|measure` refuses to run without a registered platform, or on a machine other than
the one all calibration rows came from. Measurement also needs calibration rows from that
machine. An OS update or a rebuilt binary is recorded, not blocked; verifier check Q lists
both. Dry-runs are exploratory wherever they ran. The PC's default route is Wi-Fi (pre-flight,
2026-10-23). That adds jitter to our send times, which calibration absorbs into the send
offset and the analysis treats as arrival noise. It is a property of this path, reported, not
corrected.
**Shared TPU addresses.** The address we send to is the validator's gossip `tpuQuic`. Several
validators can advertise the same IP. In the current selection, HarmonicAgave `Fd7btgyS` and
HarmonicFiredancer `JUPiTERr` both use 198.13.140.105, an IP advertised by 38 gossip nodes; BamAgave
`DRpbCBMx` uses 208.91.105.55, advertised by 9 nodes. Across all 656 candidates, 456 share a TPU
IP with another node. Validators behind one address are one **independence unit**
(PREREGISTRATION §6c): verdicts stay per validator, but anything that counts or pools validators
counts units (10 picked validators = 9 units). The address is recorded at plan time in every
manifest row (`tx.tpu_quic`) and in the selection snapshot (`shared_tpu_ips`;
`data/snapshots/shared_tpu_ips_20261022T163227Z.json` for the current one). We do not claim to
know what runs at a shared address. ASNs are not recorded.

**Send order.** Within a burst, txs go out in their planned `send_rank` order (offset first, then
rank), and the recorded `start_mono_ns` shows the actual order. Verifier check O and the parser
(`send_order_mismatch`) reject any window where the two differ. Until 2026-10-23 the burst loop
sent zero-gap arms (P, R, C) in idx order, i.e. in ascending price (P, C) or CU limit (R). That
made send order identical to the key under test. It affected dry-runs only (LOGBOOK).
Unattended runs log every blind period to `<run>/gaps.jsonl`: host sleep or stall, slot-feed
stall, RPC outage. A silent WebSocket is dropped after 10 s and reconnected, and the slot
clock is cleared after a host stall of more than 30 s. `clock_hourly.json` (per run, and
merged per campaign by `sender clock-report`) gives the error distribution by UTC hour.

### Limitations: Wi-Fi (no Ethernet)

The sending PC has no Ethernet cable, so **every run (smoke, calibration, confirmatory) goes out
over Wi-Fi** (TP-Link USB adapter). This is a stated limitation of the whole study, not a
defect that was corrected:

- A Wi-Fi hop adds delay and jitter of its own (contention, retries, power saving, frame
  aggregation), so the burst our txs form at the NIC is not exactly the burst that reaches the
  leader. Calibration absorbs the mean (the send offset); the variation stays in the data as
  arrival noise. Verdicts therefore describe ordering **as seen from this Wi-Fi path**, and a
  rule whose effect is smaller than that noise can be missed (INCONCLUSIVE, never NO_RULE
  unless the simulator's power says so at the calibrated noise).
- **What is recorded.** The pre-flight records the network type of the default route, and on
  Wi-Fi the band, channel, signal quality, RSSI (only if the driver reports a plausible one),
  PHY, link rate and the adapter's power-saving mode. It **warns and never refuses** (2.4 GHz,
  weak signal, power saving, VPN, unreadable link). During every session the supervisor
  samples the link every 30 s and measures the round-trip time to the RPC endpoint every 5 s
  (`rtt.jsonl`, `wifi.jsonl`), and writes `network.json` and a coverage-report section at the
  end (also when the session was stopped). Every manifest row carries `host.network_type`. The
  SSID is never read, and the access point appears only as a short hash of its BSSID.
- **RTT is a proxy.** It is a JSON-RPC `getSlot` over a kept-alive HTTPS connection: it
  includes the provider's processing and the internet path, not only the Wi-Fi hop, and it is
  not the delay of a QUIC send to a leader. It enters the noise analysis as a reported noise
  level and as a labeled simulator sensitivity (`analysis/slwlab/calibrate.py`,
  PREREGISTRATION §6d). It is never used to correct, drop or reweight a window.
- **Platform strata.** The stratum is (platform id, OS, machine, **network type**). A session on
  another network type (Ethernet later, a phone hotspot, a VPN) is a different stratum: the
  live gate refuses to mix it with the calibrated one, verifier check Q fails on a data set
  with two known network types, and its data would be analyzed separately and never pooled.
- **Observed on 2026-10-25 (pre-flight, EXPLORATORY).** Connected on 2.4 GHz, channel 13, 802.11n,
  150 Mbit/s, signal quality 90 %. A 150 Mbit/s 802.11n link is what a single-stream 2.4 GHz
  adapter negotiates, so 5 GHz is probably not available with this adapter (model not verified).
  The RSSI query returned a nonsense value (+247 dBm) and is now filtered.

Operator checklist before a Wi-Fi session: near the router (line of sight, signal quality at
least 60 %); 5 GHz if the adapter supports it (otherwise a quiet 2.4 GHz channel, set on the
router); adapter power saving off (power plan "Wireless Adapter Settings" = Maximum Performance
and, in Device Manager, "Allow the computer to turn off this device" unchecked); no VPN; no
downloads, uploads, cloud sync or video on the network while a session runs.

### Interaction of our own transactions with each other
In arms P, F, X and R, each tx has its own fee payer and no other writable account (tested), so
our txs never share a write lock. A payer is reused only after its previous tx is resolved. Arm C
deliberately makes all N txs write-lock one shared account. It measures scheduling under
conflict, is reported separately, and its source-derived prediction is in VERIFIED_FACTS F6b.

### Competition inside the block
_TODO._

### Send order
Send order is a controlled variable. It is seeded-random in P, R and C, fixed in F, and the
reverse of price order in X, with the gap swept over {0, 10, 25, 60, 120} ms. The seed, every
planned offset and every actual send time are recorded.

### Noise vs rule
A single window cannot say "rule" or "noise". So every verdict is about many windows, against
a simulated reference (`analysis/slwlab/sim.py`, labeled SIMULATION everywhere):
- **Ceiling.** Even a validator that follows rule R exactly does not reach tau = 1. Agave's 4
  worker threads (F6c) record batches in completion order, so our lock-free txs interleave
  (simulated ceiling ≈ 0.43 for Arm P). Firedancer's microblocks of ≤ 5 txs on 2 exec tiles give
  ≈ 0.81–0.86. tau is normalized by the ceiling of its cell (family × arm × rule).
- **Arm C as the clean test.** The shared write lock removes interleaving (ceiling ≈ 0.86–0.90),
  so Arm C separates a strict rule from a half-strength rule (simulated false-STRICT 0.00).
- **Thresholds and power** per cell come from the simulator (`docs/SIMULATION.md`). A rule
  that its arm cannot reveal (e.g. FIFO in arms without send gaps) can never be STRICT, and a
  cell without ≥ 90% power cannot be called NO_RULE.
- **Tests** are exact randomization tests over all orderings of each window, Holm-corrected
  across rules (`analysis/slwlab/stats.py`).

## What this does NOT separate
- **Hours the PC was off.** Sessions run in the daytime only (UTC ≈ 05–19), so every
  measurement covers only the UTC hours a session covered. Night hours, and any load pattern
  that only exists then, are not measured. Every session records its coverage by UTC hour,
  and the report lists windows by UTC hour. Hours without coverage are shown as gaps, never
  filled.
- **NO_RULE is structurally unreachable for Agave-family validators in the lock-free arms P
  and R** at an affordable sample size. Agave's 4 worker threads interleave our txs (simulated
  ceiling ≈ 0.43). So a half-strength rule needs ≈ 40 usable windows for 90% power, and the
  false-STRICT rate stays above 10% at W = 20. In these arms an absent effect is reported as
  INCONCLUSIVE, never as "no rule", and STRICT carries a low-separability label. The Agave
  positive control and the strictness test are therefore **Arm C** (shared write lock, one
  thread, ceiling ≈ 0.87), where both are cleanly separable in simulation.
- An internal Agave tie-break index (Slab key) cannot be told apart from random from outside
  (Arm F, TIE_NONE).
- Simulator nuisance parameters that cannot be estimated without looking at our txs' order
  (arrival jitter, execution-time noise, worker busy time) stay at preset levels. Results are
  reported across those levels.
- Window-start "prediction error" is relative to slot-notification arrival at our machine, not
  the leader's real block start.
- RPC exposes final position only, not entry or microblock boundaries.
- Arrival time at the leader is not observed directly.
- Self-reported `clientId`.
- Harmonic's strategy is not externally observable.

## Repository layout

```
crates/common    fee math, Alpenglow guard, spend ceiling (Rust)
crates/sender    planner, clock, tx builder, QUIC sender, spend ledger, sessions, CLI (Rust)
crates/winsys    the only unsafe code: keep-awake, free RAM, default-route adapter, loaded UI DLLs
analysis/        parser, candidate rules, stats, simulator, report (Python, pytest)
scripts/         verify_facts.py (re-derives docs/VERIFIED_FACTS.md from RPC), verify_manifest.py
                 (independent check of data/), check_no_keys.py, scan_history.py
data/            manifest, raw blocks, snapshots, ledger, logs (committed)
docs/            BOUNTY, VERIFIED_FACTS, PREREGISTRATION, LOGBOOK, REPORT
secrets/         payer keypairs (gitignored, never committed)
```
