# solana-leader-window-lab

**Inside the Block: How Solana Validators Order Transactions** (Superteam Ukraine bounty; full text
in [docs/BOUNTY.md](docs/BOUNTY.md)).

We send small, controlled bursts of our own transactions into the leader windows of chosen
mainnet validators. From the produced blocks we then decide which candidate ordering rule each
validator follows, how strictly it follows it, or whether it follows any rule at all.

> **Status: Phase 2 (sender, ledger, dry-run, clock) built; awaiting gate review.** No transaction
> has been sent to mainnet or devnet. No measurement exists yet. All dry-run data is exploratory.

## Quick start

```bash
cp .env.example .env              # set RPC_URL and RPC_WS_URL (env only; see "RPC provider")
docker compose up                 # select validators (if needed) + print the window plan. Never sends.
docker compose run --rm sender dry-run --duration-secs 86400   # real-time loop, nothing sent
# Windows native, unattended 24 h (restarts, gap log, hourly clock report):
#   powershell -ExecutionPolicy Bypass -File scripts\dryrun24h.ps1
docker compose run --rm analyze   # regenerate docs/REPORT.md from committed data/
docker compose run --rm verify    # re-fetch every block and check it against data/manifest.jsonl
# live (operator only; the cap is mandatory and cannot exceed 0.05 SOL):
docker compose run --rm keys init-payers --count 32
docker compose run --rm sender fund --payer-target-lamports 700000            # prints the plan
docker compose run --rm -e CONFIRM_LIVE=yes sender fund --payer-target-lamports 700000 --live --max-spend-lamports 200000
docker compose run --rm -e CONFIRM_LIVE=yes sender live --purpose calibrate --offsets -300,-100,0,100 --max-spend-lamports N
docker compose run --rm -e CONFIRM_LIVE=yes sender live --purpose measure --prereg <commit> --max-spend-lamports N
docker compose run --rm sender collect        # resolve anything left open (crash recovery)
docker compose run --rm -e CONFIRM_LIVE=yes sender sweep --live --max-spend-lamports 200000
docker compose run --rm sender reconcile --since data/snapshots/balances_<first>.json
```

`sender` subcommands: `select-validators`, `plan`, `dry-run [--probe-connect]`, `live`, `collect`,
`init-payers`, `balances`, `fund`, `sweep`, `reconcile`. Run `sender <cmd> --help` for options.
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

Order agreed at the Phase 0 gate: **2** (sender, ledger, dry-run, clock) → 3 (devnet end-to-end,
operator) → 4 (24 h mainnet dry-run, selection snapshot, clock error report, prereg frozen and
tagged `prereg-v1`) → 5 (calibration, ≤ 5,000,000 lamports, operator) → 6 (live campaigns on
≥ 3 dates) → 7 (report). Phase 1 (parser, rules, stats, simulator) runs in parallel and must be
complete before the prereg freeze.

Without Docker: `make test`, `make facts`, `make verify`.

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
Unattended runs log every blind period to `<run>/gaps.jsonl`: host sleep or stall, slot-feed
stall, RPC outage. A silent WebSocket is dropped after 10 s and reconnected, and the slot
clock is cleared after a host stall of more than 30 s. `clock_hourly.json` (per run, and
merged per campaign by `sender clock-report`) gives the error distribution by UTC hour.

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
_TODO._ Known noise source from Agave source: parallel worker threads (VERIFIED_FACTS F6).

## What this does NOT separate
_TODO._ Known now:
- RPC exposes final position only, not entry or microblock boundaries.
- Arrival time at the leader is not observed directly.
- Self-reported `clientId`.
- Harmonic's strategy is not externally observable.

## Repository layout

```
crates/common    fee math, Alpenglow guard, spend ceiling (Rust)
crates/sender    planner, clock, tx builder, QUIC sender, spend ledger, CLI (Rust)
analysis/        parser, candidate rules, stats, simulator, report (Python, pytest)
scripts/         verify_facts.py (re-derives docs/VERIFIED_FACTS.md from RPC), check_no_keys.py
data/            manifest, raw blocks, snapshots, ledger, logs (committed)
docs/            BOUNTY, VERIFIED_FACTS, PREREGISTRATION, LOGBOOK, REPORT
secrets/         payer keypairs (gitignored, never committed)
```
