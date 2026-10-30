# AMENDMENT 3 to PREREGISTRATION §6d: platform B (second operator, exploratory)

Date: 2026-10-30 (UTC). Status: DRAFT until the operator says so; no tag `amendment-3` exists yet. Written BEFORE any platform-B data:
no window of platform B has been sent. Counts only; no tau, p-value, rank or verdict was computed or read. `docs/PREREGISTRATION.md`,
the tag `prereg-v1`, `AMENDMENT_1`/`AMENDMENT_2` and their tags are unchanged. No change in `crates/`; the sender is the approved binary
`475fe47a880a` (SHA-256 `475fe47a880acab1504ea752633968dc7f1abe9abb09659d80edab663357e718`).

## 1. Purpose and status

A second operator (operator B, a friend, a Windows laptop on Wi-Fi in Kyiv) runs a SMALL session with the same sender. The aim is to look at
independence of hardware, Wi-Fi hop and operator on a similar network path (it is not a test of geography). PREREGISTRATION §6d already says a
stratum is (platform id, OS, machine id, network type) and that a different stratum is never mixed with the confirmatory data. Platform B is
such a stratum and it is **exploratory only**:

- it is never pooled with the confirmatory data (sessions 1 to 5, round 2), in any table, count, test or figure;
- it does not change a target, a quota, an allocation, a deficit or a confirmatory result;
- it is reported in its own section as "platform B (exploratory)".

## 2. Design (counts only; `docs/FRIEND_KIT/PLAN_CHECK_FULL.txt`)

| item | value |
|---|---|
| validators | the abundant ones only: 9eGrDohd (Firedancer), JUPiTERr (HarmonicFiredancer), HEL1USMZ and E1r4Psq8 (JitoAgave), Fd7btgyS (HarmonicAgave), DRpbCBMx (BamAgave) |
| arms | C, F, P, X (R not used) |
| attempted windows per validator and arm | C 4, F 4, P 4, X 5 = 17 per validator, 102 in all (X: 30 windows, 6 at each gap 0, 10, 25, 60, 120 ms) |
| allocation file | `docs/FRIEND_KIT/files/alloc_platform_b.json`, built by `scripts/friend_kit_build.py`, not edited by hand |
| selection | `docs/FRIEND_KIT/files/selection_20261030T180000Z_platformB.json`: the amendment-1 selection copy reduced to those six `picked` entries |
| cost at quota | 102 windows, 3,196,524 lamports (C 31,658; F 30,300; P 31,658; X 31,658 per window); cap x 1.05 = 3,356,350 |
| smoke calibration (needed by the host gate, `host.rs gate_live`: a measurement needs calibration rows on that platform) | `session --mode calibrate --hours 0.34 --max-spend-lamports 300000`, as in our own protocol |
| measurement | `session --mode measure --hours 3.5 --max-spend-lamports 3356350 --prereg 05a3b2338f324021b779b80ae1122b226b657fb1 --allocation ...`; `--prereg` is only a recorded string of at least 7 characters (`main.rs:1049`) |
| supply | leader windows an hour (saved mainnet schedules): 9eGrDohd 67.3, JUPiTERr 92.8, E1r4Psq8 102.8, DRpbCBMx 118.7, HEL1USMZ 152.4, Fd7btgyS 159.7; hours at supply for 17 windows: 0.25, 0.18, 0.17, 0.14, 0.11, 0.11 (bottleneck 9eGrDohd, 0.25 h) |
| duration | rate-bound, not supply-bound: the unpaced plan fits 102 windows into about 25 min; the session paces them over the 3.5 h (spend line = cap), which spreads them over several UTC hours |

Capital (not spend): 32 payers at the payer target 771,101 (650,240 rent minimum + ceil(3,656,350 / 32) + 6,600, cap = smoke 300,000 + measurement 3,356,350)
= 24,675,232, plus the Arm C sink at the rent minimum 650,240, plus 33 fund transfers at 5,000 = 165,000: 25,490,472 lamports. The master of operator B is funded with
30,000,000 lamports (0.03 SOL); `sweep` returns every payer and sink balance to it. The fund dry run prints the real figures; they decide.

## 3. Money, and the two platforms never overlap

Hard ceiling 50,000,000 lamports for the whole project (fees; `limits.rs`). The ceiling is a constant per ledger: the ledger of operator B does not know ours, so the
combined bound is kept by arithmetic and by the `--max-spend-lamports` of each session:

| item (fees, lamports) | amount |
|---|---|
| our spend so far (ledger: calibrate 4,843,462; measure 30,030,324; fund 965,000) | 35,838,786 |
| round 2 cap + its fund fees at most (AMENDMENT 2) | 3,225,611 + 160,000 = 3,385,611 |
| platform B: smoke cap 300,000 + measurement cap 3,356,350 | 3,656,350 |
| platform B: 33 fund transfers + 33 sweep transfers at 5,000 each, at most | 330,000 |
| our transfer to the master of operator B | 5,000 |
| **combined at the caps** | **43,215,747** <= 48,000,000 (reserve rule) < 50,000,000 (margin to the ceiling 6,784,253) |

If round 2 or platform B is stopped early, the spent figure is lower; if a cap is raised, this amendment is void until a new dated amendment. Operator B cannot lose more
than the 30,000,000 lamports on his master.

**No overlap in time.** Both platforms would aim at the same leader windows of the same validators, and two senders at the same moment would be a treatment on each other.
Rule: the interval of platform B's two sessions (smoke start to measurement end) and the interval of any session of the confirmatory platform are separated by at least 30 minutes,
checked after the fact from `data/sessions/<id>/session.json` (start and end, UTC) of both; an overlap is reported and the platform-B windows inside it are marked, not used.
Platform B runs only after round 2 has ended (or on another day); the order is decided by the operator.

## 4. What is compared afterwards, and how

Descriptive agreement only, no pooling, no test. For each cell of platform B (validator x arm, X by gap) a table of counts side by side with the same cell of the confirmatory platform:
attempted windows, landed windows, usable windows (the §9 rule, unchanged), usable rate. Agreement is described as counts: in how many of the cells with at least 4 attempted
windows on both platforms the arm that lands first / most often is the same. No tau, no p-value, no confidence interval, no rank test, no verdict is computed on platform-B data;
if the operator wants one later it needs a new dated amendment. A difference is a description of two strata, not evidence of a cause (hardware, Wi-Fi hop, operator and
time of day are confounded by design).

## 5. Data return and verification (counts only)

Operator B zips his `data/` folder (never `secrets/`, never `.env`) and sends it. We run `scripts/friend_data_check.py DATA_B.zip`: members only under `data/`; secret scan with the
tests of `check_no_keys.py` (API key value, keypair array, base58 keypair); `scripts/verify_manifest.py --data <dir>` offline, including check Q (one platform, one machine, one network type);
counts. It extracts to `data_platform_b/<UTC stamp>/data/`, a directory the confirmatory scripts never read (`data/` is hard-wired in `supplementary_allocation.py`, `round2_allocation.py`,
`session_cmd*.py`, `check_no_keys.py`). The blocklists (`slwlab.blocklist fetch --data data_platform_b/<stamp>/data --manifest ...`) are fetched by us with our read-only RPC key,
because operator B has no Python. No code in `analysis/` changes: `parse.py`, `analyze.py` and `stats.py` contain no platform field, so pooling can only happen by passing both manifests to
one call, which this amendment forbids; check Q of `verify_manifest.py` (line 348) also fails on a manifest that holds two platforms.

## 6. Files (SHA-256)

| file | SHA-256 |
|---|---|
| `docs/FRIEND_KIT/files/alloc_platform_b.json` | `ee508173fabf9f66cec225f2490a0a27b2ba7bbead580a702f28f99c686558b9` |
| `docs/FRIEND_KIT/files/selection_20261030T180000Z_platformB.json` | `5cb996adb573abf4646feb8b5a66c41a39aae87f0b2ea99d191e3d1cbc7b1385` |
| `docs/FRIEND_KIT/CHECKSUMS.txt` (lists every member of the kit zip; the zip itself is not committed) | in the LOGBOOK entry |
| `scripts/friend_kit_build.py`, `scripts/friend_data_check.py` | in the LOGBOOK entry |
| sender binary | `475fe47a880acab1504ea752633968dc7f1abe9abb09659d80edab663357e718` |

## 7. Limitations

One laptop, one night, 102 windows: no cell reaches the confirmatory targets, and none is meant to. The unpaced plan is dense (6 txs per window, windows 5 s apart at the earliest) and the session
spreads it; the Helius free plan limits (8 requests a second, 1 sendTransaction a second) apply to operator B exactly as to us. Operator B's clock offset is only warned about by the pre-flight
(50 ms), not refused. The master key of operator B is generated by us and travels to him; it holds 0.03 SOL at most.
