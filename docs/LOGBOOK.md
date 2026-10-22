# Logbook

Append-only. UTC timestamps. Every surprise, failure, dead end, rate limit, missed window.
This file feeds the "What went wrong" section of docs/REPORT.md.

| UTC | Phase | Kind | Entry |
|---|---|---|---|
| 2026-10-22 15:05 | 0 | surprise | Epoch 1064 is nearly over at first contact (slotIndex 431,634 / 432,000). Epoch 1065 starts at slot 460,080,000. |
| 2026-10-22 15:10 | 0 | surprise | Alpenglow did not start at the epoch-1064 boundary (459,648,000). The last block with vote txs is 459,652,999, slots 459,653,000–007 were skipped, and the first block without votes is 459,653,008. The brief's slot constant is right; "activated at epoch 1064" is only approximately right. |
| 2026-10-22 15:12 | 0 | surprise | Slot time was already ~215 ms *before* Alpenglow, so it cannot serve as a boundary marker. |
| 2026-10-22 15:14 | 0 | failure | `getBlock` with `maxSupportedTransactionVersion: 0` fails on post-boundary blocks because of v1 transactions. We must use 1. |
| 2026-10-22 15:16 | 0 | rate limit | The public RPC returned "Too many requests for a specific RPC call" after about 4 sequential `getBlock` calls. `verify_facts.py` retries with backoff; the campaign will need a private RPC (`RPC_URL`). |
| 2026-10-22 15:25 | 0 | surprise | Agave v4.3.2 has no prio-graph scheduler, only the greedy scheduler. Ties on priority are broken by slab index, not arrival time. |
| 2026-10-22 15:30 | 0 | surprise | Agave's cost includes 8 CU per 32 KiB of the *requested* loaded-accounts-data limit (default 64 MiB = 16,384 CU). We must set the limit explicitly or R_RATIO is dominated by this term. |
| 2026-10-22 15:40 | 0 | surprise | `getClusterNodes` exposes a `clientId` field (AgaveBam, HarmonicAgave, JitoLabs, Firedancer, HarmonicFiredancer, FireBAM, Rakurai, Agave). Vanilla Agave holds only ~1.0% of stake. |
| 2026-10-22 15:42 | 0 | surprise | Rent-exempt minimum for 0 bytes is 650,240 lamports (not 890,880). |
| 2026-10-22 15:50 | 0 | env | Docker and gitleaks are not installed on the dev machine (Windows). Container builds and gitleaks run only in CI until they are installed locally. |
