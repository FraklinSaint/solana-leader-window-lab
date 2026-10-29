> SUPERSEDED on 2026-10-29 by docs/AMENDMENT_1_SUPPLEMENTARY.md (cap full, no futility test, no X gap restriction, selection copy without EvnRmnMr). Kept as the draft history; its text below is NOT in force.

# PROPOSAL (NOT REGISTERED): amendment to §8 "Supplementary sessions"

Status: updated draft of 2026-10-29 (first draft 2026-10-28, commit `276857e`), for the operator's decision.
`docs/PREREGISTRATION.md` is not edited by this file and nothing here is committed as an amendment yet. If
accepted, the text below replaces the sentence "Its quota per cell is `max(0, target - usable windows)`" in the
supplementary procedure, is added to PREREGISTRATION §8 with its date, and `scripts/supplementary_allocation.py` is
committed with its SHA-256 in the LOGBOOK BEFORE any supplementary session starts. It does not change the allocation
files of sessions 1 to 4 and does not apply to any session already run.

## Why (counts only)

The registered quota `max(0, target - usable)` has no upper bound per cell and no way to stop a cell that cannot
produce usable windows for a reason outside the experiment. After sessions 1 to 4 (847 measure windows attempted, 635
usable, `docs/LOGBOOK.md` 2026-10-29):

- **EvnRmnMr** (Firedancer): 0 usable of 75 attempted windows, 450 of 450 txs failed at the connection, in all five arms.
- **X at 60 and 120 ms**: 6 usable of 36 attempted at 60 ms, 0 of 36 at 120 ms (33 each without EvnRmnMr). At 0, 10 and 25 ms
  the counts are 32 of 44, 28 of 36, 24 of 36 (32 of 40, 28 of 33, 24 of 33 without EvnRmnMr).
- Sum of cell deficits 217 usable windows (X 95); 39 attempted windows of the 4of4 file are still unplanned (5Us18hLZ 32,
  7QQGNm3p 7).

## Proposed text

**Supplementary allocation with a cap, a gap restriction and a validator exclusion (outcome-blind).** The allocation script
reads ONLY, per cell (validator x arm; pooled Agave as registered; X also by gap): the registered target `T` of usable
windows (`W_usable_per_validator` x validators of `alloc_v2.json`, scenario `calibrated`), the number of usable windows `U`
(§9, stored getBlocks lists), the number of attempted windows `A` (every window of purpose `measure`, usable or not), and the
cell's planned attempted quota `Q` per validator of `alloc_calibrated_session4of4.json`. It reads only from a data set whose
newest `data/verify/verify_*.json` is ok and covers every manifest line. It takes no tau, p-value, rank, verdict or arm
comparison as input and computes none.

1. **Quota.** The quota of a cell, in attempted windows, is `min(cap, max(0, T - U))`.
2. **Cap.** The cap of a cell is `n x S` where `n` is the number of validators measured in the cell and `S` the cap per
   validator over all supplementary sessions together. The operator fixes `S` before the supplementary file is committed
   and records it in the LOGBOOK: `S = ceil(Q / 2)` ("half") or `S = Q` ("full", the full planned quota). The script writes
   both files (`alloc_supplementary_halfcap.json`, `alloc_supplementary_fullcap.json`); only one is used.
3. **X gaps.** Supplementary X windows are planned only at gaps 0, 10 and 25 ms, the quota split evenly over the three gaps
   (the remainder to the lowest gaps first). Reason, counts only: 60 ms has 6 usable of 36 attempted and 120 ms has 0 of 36,
   so more windows there cannot close the X cell. 60 and 120 ms keep their counts in REPORT.md and stay exploratory; no value
   is substituted.
4. **Excluded validator.** EvnRmnMr gets no supplementary window: 0 usable of 75 attempted (futile). The Firedancer cells are
   measured by 9eGrDohd alone (`n = 1`); their targets stay the registered `T` (pooled for two validators), so each Firedancer
   cell asks 9eGrDohd for up to `S` more windows. The sensitivity with `T = W` (one validator's share) is in the dry run. EvnRmnMr's
   cells keep their counts in REPORT.md as "target not reached: 0 of T usable, 75 attempted"; the saved budget stays in the pool of
   the other cells.
5. **Cumulative file.** The allocation file has the format of `alloc_calibrated_session{K}of4.json`. Its `plan_quota` is the 4of4
   quota plus the supplementary quota per validator, because the planner asks for `quota - done` per validator and arm; the 39
   unplanned windows of 4of4 are therefore asked again by the same file.
6. **Timing.** The numbers are evaluated once, after session 4 is finished and verified (2026-10-29), from the counts at that moment,
   and written to the LOGBOOK before the file is committed. They are not re-evaluated to rescue a cell later.
7. **Date.** This rule is dated 2026-10-29 and committed before any supplementary session. The counts it uses are the usable and
   attempted windows of sessions 1 to 4 only, which are fixed by the registered allocation files and not by the rule.

## What the rule does not do (stated, not decided)

- The quota is in attempted windows and equals the deficit; at the observed usable rates (P 88.2%, F 89.5%, R 98.9%, C 91.3%, X
  79.3% at 0 to 25 ms, without EvnRmnMr) closing a deficit `D` takes `D / rate` attempts, about 265 attempted windows against a quota of
  179 (half cap) or 212 (full cap). So the cells whose cap does not bind close only about 80 to 90% of their deficit in expectation.
  `docs/SUPPLEMENTARY_DRYRUN_FULL.txt` shows both numbers per cell.
- The cell-level futility test of the 2026-10-28 draft (`A >= 10` and `T - U > S x U / A`) is not part of this text: applied (with `S` per validator), it
  would drop 9 cells at the half cap (Agave X, BamAgave X, HarmonicAgave X, HarmonicFiredancer X, JitoAgave X, Rakurai P, F, X, C) and 6 at the full
  cap (Agave X, BamAgave X, JitoAgave X, Rakurai P, X, C). It is listed as information only in the dry run.
- **Sender limits (no code was changed).** EvnRmnMr can be left out without a code change by passing `--selection` a copy of the
  selection snapshot without it (a new file). The X gap restriction cannot: the planner rotates X windows over
  `x_gaps_ms = [0, 10, 25, 60, 120]` (`planner.rs` line 285, `arms.rs`); restricting it is an engineering change in `crates/`
  (new binary hash) and is a separate decision. The keys `x_gap_quota_per_validator` and `excluded_validators` in the allocation files
  are documentation, not read by the sender.
- The allocation files of sessions 3 and 4 still contain EvnRmnMr (they were run with it; changing them would deviate from the tagged files).
- A contact-address refresh during a session is an engineering change independent of this rule (see `docs/RAKURAI_DELIVERY_FULL.txt`).
