# AMENDMENT 1 to PREREGISTRATION §8: supplementary round

Date: 2026-10-29 (UTC). Status: decided by the operator, outcome-blind, from COUNTS only (usable and attempted windows of
sessions 1 to 4, `data/verify/verify_20261029T102026Z.json` ok). No tau, p-value, rank or verdict was computed or read.
`docs/PREREGISTRATION.md` and every file of the tag `prereg-v1` are unchanged; this amendment is pinned by the tag
`amendment-1`. It supersedes `docs/PROPOSAL_SUPPLEMENTARY_AMENDMENT.md` (draft of 2026-10-28/29). It does not change the
allocation files of sessions 1 to 4 and applies to no session already run. No further supplementary round without a new
dated amendment.

## 1. Rule

1. **Quota per cell** (validator x arm; pooled Agave as registered), in attempted windows:
   `quota = max(0, registered target - usable windows)` with the registered target
   `W_usable_per_validator x validators` of `alloc_v2.json` (scenario `calibrated`) and the usable windows of §9 counted over
   sessions 1 to 4.
2. **Cap: FULL.** At most `S = Q` supplementary attempted windows per validator and cell (Q = the planned attempted quota per
   validator in `alloc_calibrated_session4of4.json`), cumulative over ALL supplementary sessions together. The half-cap file
   (`S = ceil(Q/2)`) was computed for comparison and is NOT used.
3. The numbers (counts, deficits, quotas per cell) are fixed by `scripts/supplementary_allocation.py` and shown in
   `docs/SUPPLEMENTARY_DRYRUN_FULL.txt`: 214 supplementary attempted windows (212 cell quota before rounding up to whole
   per-validator quotas), 6,765,412 lamports; plus the 39 windows of 4of4 still unplanned (1,244,134 lamports).
4. The cumulative allocation file holds `plan_quota = 4of4 quota + supplementary quota per validator`. The planner asks for
   `quota - done` per validator and arm (`planner.rs`).

## 2. Firedancer: 9eGrDohd alone

The Firedancer cells keep the registered pooled target (for example 30 usable windows in P, F, X, C and 20 in R) and are
measured by `9eGrDohd` alone, because `EvnRmnMr` has 0 usable windows of 75 attempted over four sessions (450 of 450 txs failed
at the connection, in all five arms). **Findings for the Firedancer stratum are therefore findings about `9eGrDohd` only**; they
are not findings about Firedancer validators in general, and EvnRmnMr's cells are reported as "target not reached: 0 of T usable,
75 attempted".

## 3. EvnRmnMr is excluded from the supplementary round

`sender session` has no `--selection` flag and its workers pass none (`main.rs` `Cmd::Session`, `session::worker_args`); `live`
loads the newest `data/snapshots/selection_*.json` by file name (`latest_selection`). The exclusion is therefore done with a copy
of the selection file without EvnRmnMr, with no code change:

| file | SHA-256 |
|---|---|
| original `data/snapshots/selection_20261022T163227Z.json` (unchanged) | `877286d5dc6dc6e6c6e712a99b25ca3b7c1069822a530c85a353849c7335f2fe` |
| copy `analysis/sim/results/sessions/selection_amendment1_no_EvnRmnMr.json` | `4a879aff21f252ded61a9546b72824e16fb5d6655b5398e29b8d9bf53ae963ed` |

The copy is stored in the repo under that path. It is NOT kept in `data/snapshots/` between sessions, because tagged tools and
tests read the newest `selection_*.json` there (a copy placed there made 4 tests of `test_session_cmd.py` fail). Just before the
session it is copied byte for byte to `data\snapshots\selection_20261029T112100Z_amendment1_no_EvnRmnMr.json` (sorts after the
original) and its SHA-256 is checked (`docs/SESSION_5_COMMANDS.txt`). Diff against the original (the `picked` entry of
EvnRmnMr and one note; `candidates` keeps all evidence):

```diff
--- selection_20261022T163227Z.json
+++ selection_amendment1_no_EvnRmnMr.json
@@ -14058,3 +14058,5 @@
   "min_windows": 40,
-  "notes": [],
+  "notes": [
+    "Copy of selection_20261022T163227Z.json for docs/AMENDMENT_1_SUPPLEMENTARY.md (2026-10-29): the picked validator EvnRmnMr is removed (0 usable of 75 attempted windows over four sessions). Nothing else changed."
+  ],
   "picked": [
@@ -14136,25 +14138,2 @@
     },
-    {
-      "client_id": "Firedancer",
-      "confidence": "medium",
-      "evidence": [
-        "gossip clientId=Some(\"Firedancer\")",
-        "version=Some(\"26.10.4\")",
-        "kobe running_jito=Some(true) running_bam=None bam_connection_rate=None",
-        "running_jito: Firedancer bundle tile enabled (bundles are out of scope)",
-        "Kobe lists the validator but has no deciding field"
-      ],
-      "identity": "EvnRmnMrd69kFdbLMxWkTn1icZ7DCceRhvmb2SJXqDo4",
-      "kobe": {
-        "bam_connection_rate": null,
-        "running_bam": null,
-        "running_jito": true
-      },
-      "stake_lamports": 7797558581163021,
-      "stratum": "Firedancer",
-      "tpu_quic": "104.204.143.114:9007",
-      "version": "26.10.4",
-      "vote_account": "9QU2QSxhb24FUX3Tu2FpczXjpK3VYrvRudywSZaM29mF",
-      "windows_in_horizon": 3679
-    },
     {
```

## 4. Futility test: not used

The cell-level futility test of the 2026-10-28 draft (`A >= 10` and `T - U > S x U / A`) is NOT part of this amendment. Applied,
it would have dropped 6 cells at the full cap (Agave X, BamAgave X, JitoAgave X, Rakurai P, X, C); all of them stay in.

## 5. X gaps are not restricted (no code change); expected consequence

The planner rotates the X windows over `x_gaps_ms = [0, 10, 25, 60, 120]` (`planner.rs` line 285, `arms.rs`); restricting it would
need a change in `crates/`, which is not made. Consequence, in numbers, from the plan check
(`docs/SUPPLEMENTARY_PLAN_CHECK_FULL.txt`): of 99 X windows planned (including 7 of the 4of4 leftover), 38 (38.4%) fall at 60 or
120 ms (60 ms: 20, 120 ms: 18), where the observed usable counts are 6 of 36 and 0 of 36 attempted (6 of 33 and 0 of 33 without
EvnRmnMr). At the observed usable rates per gap (0 ms 32/40, 10 ms 28/33, 25 ms 24/33, 60 ms 6/33, 120 ms 0/33) the 99 X windows
give about 52 usable windows (3.6 of them from 60/120 ms) against an X deficit of 95; the expected shortfall per X cell is about
Agave 4.6, BamAgave 6.1, Firedancer 12.5, HarmonicAgave 3.7, HarmonicFiredancer 2.6, JitoAgave 8.4, Rakurai 5.0 (about 43 in all).
**These X cells are expected to stay below their target**; this is reported as a limitation, not corrected.

## 6. The 4of4 leftover (39 windows)

The registered 4of4 leftover (5Us18hLZ 32, 7QQGNm3p 7; 1,244,134 lamports) is inside the same cumulative file and runs in the same
session, planned together with the supplementary windows. The sender cannot give it priority inside one file: the planner has one
cumulative quota per validator and arm and picks, in time order, the arm with the largest remaining share
(`pick_quota_arm`); leftover and supplementary windows are not told apart (`docs/SUPPLEMENTARY_PLAN_CHECK_FULL.txt`, section 4).
A session that needs the leftover to come first runs it alone with `alloc_calibrated_session4of4.json` (a registered file) before
the combined file; both are listed in `docs/SESSION_5_COMMANDS.txt`.

## 7. What does not change

- The confirmatory usable-window rule (§9) is unchanged. Original and supplementary windows are pooled, and each window is labelled
  by its session: the `run_id` of the manifest line and the session record `data/sessions/<id>/session.json` (field `allocation`
  names the file used), listed per session in `docs/SESSIONS.md` and REPORT.md.
- Budgets: calibration <= 5,000,000, confirmatory <= 38,000,000, total <= 48,000,000, hard ceiling 50,000,000 lamports. Spent
  so far (ledger, fees paid): calibrate 4,843,462; measure 22,330,884; fund 805,000; total 27,979,346. Plan of this amendment:
  measure +8,009,546 (cap level at most x 1.05 = 8,409,524) gives 30,740,408 <= 38,000,000; with two funding top-ups of 160,000
  the total is at most 27,979,346 + 8,409,524 + 320,000 = 36,708,870 <= 48,000,000 < 50,000,000.

## 8. Files (SHA-256)

| file | SHA-256 |
|---|---|
| `analysis/sim/results/sessions/alloc_supplementary_fullcap.json` (used) | `b56395ee233030549e682493b9a37fac536148a89a8a702190da412ef61ccdda` |
| `analysis/sim/results/sessions/alloc_supplementary_halfcap.json` (comparison, not used) | `f7826620bc4fd7a98cb227dbf949f610c1e08f7a75ae8329b181c9b8c9d6c271` |
| `analysis/sim/results/sessions/selection_amendment1_no_EvnRmnMr.json` | `4a879aff21f252ded61a9546b72824e16fb5d6655b5398e29b8d9bf53ae963ed` |
| `scripts/supplementary_allocation.py` | `f40d833e841888bcd9a91984b75ab53f27bd292d3652ea400589c4d93b7ee8b9` |
| `scripts/session_cmd_supp.py` (cap and payer target; reuses the tagged `scripts/session_cmd.py`, untouched) | `f7bd4565afa5d32dccde5dbd8e7da1d3664f89f2f9999326ec36fb116db138ce` |
| sender binary `target\release\sender.exe` (475fe47a880a) | `475fe47a880acab1504ea752633968dc7f1abe9abb09659d80edab663357e718` |

The script reads only per-cell counts (usable, attempted, targets, planned quotas) from data whose newest `verify_*.json` is ok;
`python scripts/supplementary_allocation.py --check` regenerates both allocation files and the dry run and compares them with the
files on disk.

## 9. Limitations

Remaining deficits are reported as they are. At the observed usable rates closing every deficit would take about 265 attempts
against a quota of 212 (full cap); the cells whose cap does not bind close about 80 to 90% of their deficit in expectation. The
Rakurai cells (`ana2y2Yv`: 37 usable of 84 attempted, with a delivery problem at 45.32.145.112:8002 documented in
`docs/RAKURAI_DELIVERY_FULL.txt`) and the Firedancer cells (one validator) are expected to stay short. No result of a cell that
misses its target is presented as confirmatory evidence of the target power.
