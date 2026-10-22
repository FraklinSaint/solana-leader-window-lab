# solana-leader-window-lab

**Inside the Block: How Solana Validators Order Transactions** (Superteam Ukraine bounty; full text
in [docs/BOUNTY.md](docs/BOUNTY.md)).

We send small, controlled bursts of our own transactions into the leader windows of chosen
mainnet validators. From the produced blocks we then decide which candidate ordering rule each
validator follows, how strictly it follows it, or whether it follows any rule at all.

> **Status: Phase 0 (scaffold + verified facts).** No transaction has been sent. No measurement exists yet.

## Quick start

```bash
cp .env.example .env              # set RPC_URL (a private RPC is strongly recommended)
docker compose up                 # dry-run plan on mainnet. Never sends.
docker compose run --rm analyze   # regenerate docs/REPORT.md from committed data/
docker compose run --rm verify    # re-fetch every block and check it against data/manifest.jsonl
# live (operator only; the cap is mandatory and cannot exceed 0.05 SOL):
docker compose run --rm -e CONFIRM_LIVE=yes sender live --max-spend-lamports N
```

Without Docker: `make test`, `make facts`, `make verify`.

## Safety invariants

- **Dry-run by default.** Live mode requires `live`, `CONFIRM_LIVE=yes` and an explicit
  `--max-spend-lamports N`, with `N ≤ HARD_CEILING_LAMPORTS = 50_000_000` (a compile-time constant).
- **Alpenglow guard.** The code refuses any slot `< 459_653_000` (see VERIFIED_FACTS F1). Live
  plans additionally require `epoch ≥ 1065`.
- **Keys** come only from `./secrets` (gitignored, mounted read-only) or env. CI runs gitleaks and
  a keypair-pattern check.
- **No Jito bundles, tips or block-engine endpoints.** Plain transactions only.

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
_TODO (Phase 2/4)._

### Path from our backend to the executing node, and everything in between
_TODO (Phase 2/5)._ Known now: for BAM validators the TPU address may route to a BAM node
(VERIFIED_FACTS F7). Harmonic FIFO timestamps arrival "at the builder".

### Interaction of our own transactions with each other
_TODO._ Each tx has its own fee payer and no other writable account, so there are no shared write locks.

### Competition inside the block
_TODO._

### Send order
_TODO._

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
