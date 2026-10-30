# AMENDMENT 2 to PREREGISTRATION §8: round 2 (deficit cells only)

Date: 2026-10-30 (UTC). Status: decided by the operator, outcome-blind, from COUNTS only (usable and attempted windows of
sessions 1 to 5, `data/verify/verify_20261030T085225Z.json` ok, 1,093 measure windows). It is written BEFORE any round-2 data:
no window of round 2 has been sent. No tau, p-value, rank or verdict was computed or read. `docs/PREREGISTRATION.md`, every
file of the tag `prereg-v1`, `docs/AMENDMENT_1_SUPPLEMENTARY.md` and the tag `amendment-1` are unchanged. This amendment is
pinned by the tag `amendment-2`. **There is no round 3**: whatever deficit is left after round 2 is reported as it is.

## 1. Rule

1. **Cells.** Round 2 covers only the cells whose usable windows U are below the registered target T (the
   `W_usable_per_validator x validators` of `alloc_v2.json`, scenario `calibrated`; pooled Agave as registered), counted over
   sessions 1 to 5, EvnRmnMr excluded (§3). Recomputed from the verified data (`docs/ROUND2_DRYRUN_FULL.txt`):

   | cell | T | U | A | deficit | usable rate U/A | attempts = ceil(deficit / rate) |
   |---|---|---|---|---|---|---|
   | Agave X | 40 | 36 | 60 | 4 | 0.600 | 7 |
   | BamAgave P | 25 | 24 | 33 | 1 | 0.727 | 2 |
   | BamAgave X | 20 | 15 | 34 | 5 | 0.441 | 12 |
   | Firedancer X | 30 | 17 | 32 | 13 | 0.531 | 25 |
   | HarmonicAgave X | 20 | 16 | 29 | 4 | 0.552 | 8 |
   | HarmonicFiredancer X | 15 | 12 | 22 | 3 | 0.545 | 6 |
   | JitoAgave X | 40 | 31 | 60 | 9 | 0.517 | 18 |
   | Rakurai X | 20 | 15 | 32 | 5 | 0.469 | 11 |

   The deficits are the ones named in the order of this round (Agave X 4, BamAgave P 1, BamAgave X 5, Firedancer X 13,
   HarmonicAgave X 4, HarmonicFiredancer X 3, JitoAgave X 9, Rakurai X 5): 44 usable windows. All other cells have no deficit.
2. **Attempts per cell** = deficit divided by the observed usable rate of the cell (counts only; X over all five gaps), rounded
   up: 89 attempts. The rate is that of the validators that stay in the round (EvnRmnMr out, §3).
3. **The 7 missing windows of 5Us18hLZ** (quota of `alloc_supplementary_fullcap.json` not reached in session 5: C 1, F 1, P 2,
   R 1, X 2) stay in the file. Arithmetic of the order: 89 + 7 = 96 windows.
4. **Windows of the file: 97, not the 108 of the order.** Recomputed, two things differ, both from the rule above and not from a choice:
   - Firedancer X: the earlier estimate (37 attempts) used the rate 17/48 = 0.354, which includes the 16 failed windows of
     EvnRmnMr. EvnRmnMr is excluded from round 2, so the rate of the validator that stays in (9eGrDohd, 17/32 = 0.531) is used: 25 attempts.
   - Agave X: the planner gives every validator of a pool the same quota, so 7 attempts become 2 x 4 = 8 (+1 window).
   - The 2 windows of Agave X left open for 5Us18hLZ also count toward the Agave X deficit; they are not subtracted.
5. **Allocation file** `plan_quota = plan_quota of alloc_supplementary_fullcap.json + ceil(attempts / validators)` per validator in
   a cell in deficit (the planner asks for `quota - done` per validator and arm, `planner.rs`). Windows per validator x arm:
   5Us18hLZ 11 (C 1, F 1, P 2, R 1, X 6), 7QQGNm3p X 4, 9eGrDohd X 25, DRpbCBMx 14 (P 2, X 12), E1r4Psq8 X 9, HEL1USMZ X 9,
   Fd7btgyS X 8, JUPiTERr X 6, ana2y2Yv X 11; total 97 windows, 3,072,010 lamports.
6. **X gaps are not restricted** (no code change; `planner.rs` rotates 0, 10, 25, 60, 120 ms). About 40% of the X windows fall at 60 or
   120 ms (plan check: 35 of 90 X windows, 38.9%), where usable is low (sessions 1 to 5, EvnRmnMr out: 60 ms 11 of 53, 120 ms
   0 of 50 attempted; 0 ms 48 of 57, 10 ms 50 of 56, 25 ms 33 of 53). The cell rates used above are the observed ones over all five
   gaps, so this is already inside the attempt counts; the X cells may still stay short.

## 2. Timing and budget

Session of round 2: one daytime session, light 16:00 to 20:00 local (UTC+2), ending by 19:30 local; `--hours H` with `HA = H`
(cap and payer target do not depend on H then). At the sender rate seen (44 windows an hour in session 4; 27.3 an hour in session 5,
which was paced over 9 h) 97 windows take 2.2 h or more, so the quota may not be reached; the windows missed are the late ones.
Leftover deficits are reported as they are.

Budgets (lamports, ledger, fees paid): calibration 4,843,462 <= 5,000,000; confirmatory 38,000,000; total 48,000,000; hard ceiling 50,000,000.
Spent so far: calibrate 4,843,462; measure 30,030,324; fund 965,000; total 35,838,786; open reservations: none counted.

| item | lamports |
|---|---|
| round 2 at quota (97 windows) | 3,072,010 |
| round 2 cap (x 1.05) | 3,225,611 |
| fund top-up fees (32 x 5,000) at most | 160,000 |
| confirmatory: 30,030,324 + 3,225,611 | 33,255,935 <= 38,000,000 (margin 4,744,065) |
| total: 35,838,786 + 3,225,611 + 160,000 | 39,224,397 <= 48,000,000 (margin 8,775,603) < 50,000,000 |

Payer target 757,641 (650,240 + ceil(3,225,611 / 32) + 6,600). The top-up itself is a transfer between the operator's own wallets, not a cost.

## 3. EvnRmnMr and the selection mechanism

EvnRmnMr stays excluded (0 usable windows of 75 attempted in four sessions; Firedancer findings remain findings about 9eGrDohd only,
`docs/AMENDMENT_1_SUPPLEMENTARY.md` §2). The mechanism is the one of session 5 and unchanged: `sender session` has no `--selection`
flag and loads the newest `data/snapshots/selection_*.json`; the copy without EvnRmnMr (same file as amendment 1, SHA-256 below)
is copied byte for byte to `data\snapshots\selection_20261030T120000Z_amendment2_no_EvnRmnMr.json` (sorts after the original and after the
amendment-1 name) just before the session and REMOVED by the close-out (`docs/SESSION_6_COMMANDS.txt`), because tagged tools and tests
read the newest selection there.

## 4. What does not change

- The confirmatory usable-window rule (§9) is unchanged. Windows of the original design, the supplementary round and round 2 are
  pooled, each labelled by its session (`run_id` of the manifest line; `data/sessions/<id>/session.json`, field `allocation` names the file).
- No change in `crates/`; the sender binary is the one of amendment 1.
- Counts, caps, quotas and the order of tests do not depend on any outcome. No round 3.

## 5. Files (SHA-256)

| file | SHA-256 |
|---|---|
| `analysis/sim/results/sessions/alloc_round2.json` (allocation of round 2) | `ff1be2145ca017e96e7ec8557a3207e7d86605de1699d0b385352ce10ef5f4d6` |
| `analysis/sim/results/sessions/selection_amendment1_no_EvnRmnMr.json` (selection copy, unchanged from amendment 1) | `4a879aff21f252ded61a9546b72824e16fb5d6655b5398e29b8d9bf53ae963ed` |
| `scripts/round2_allocation.py` (builds the allocation file and the dry run; `--check`) | `d32ea0d3cc36599f93ad9a681df80da8674e60b056467528987392800def69a0` |
| `scripts/session_cmd_supp.py` (cap and payer target; unchanged since amendment 1; used with `--alloc alloc_round2.json`) | `f7bd4565afa5d32dccde5dbd8e7da1d3664f89f2f9999326ec36fb116db138ce` |
| sender binary `target\release\sender.exe` (475fe47a880a) | `475fe47a880acab1504ea752633968dc7f1abe9abb09659d80edab663357e718` |

Dry run: `docs/ROUND2_DRYRUN_FULL.txt`; plan check with `sender plan` (cannot send): `docs/ROUND2_PLAN_CHECK_FULL.txt` (97 windows,
3,072,010 lamports, EvnRmnMr windows 0). The SHA-256 of these two files are in the LOGBOOK entry of the commit after this one.

## 6. Limitations

The X cells are expected to stay below target (the rates above are low and 60/120 ms windows are about 40% of the X windows); the
Rakurai cells (`ana2y2Yv` delivery problem, `docs/RAKURAI_DELIVERY_FULL.txt`) and the Firedancer cells (one validator) likewise. No result of a
cell that misses its target is presented as confirmatory evidence of the target power.
