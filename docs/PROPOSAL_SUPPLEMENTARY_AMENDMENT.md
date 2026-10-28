# PROPOSAL (NOT REGISTERED): amendment to §8 "Supplementary sessions"

Status: a draft written on 2026-10-28 for the operator's decision. `docs/PREREGISTRATION.md` is
not edited by this file. If accepted, the text below replaces the sentence "Its quota per cell is
`max(0, target - usable windows)`" in the supplementary procedure, is added to PREREGISTRATION §8
with its date, and the script that implements it is committed with its SHA-256 in the LOGBOOK
BEFORE any supplementary session starts. It does not change the allocation files of sessions 3
and 4, and it does not apply to any session already run.

## Why

The registered quota `max(0, target - usable)` has no upper bound per cell. A cell that cannot
produce usable windows for a reason outside the experiment (a validator that refuses our
connection, a stale contact address, a leader that rarely leads) would receive windows without
limit and drain the 38,000,000 lamport confirmatory budget at the expense of the cells that can
still close. After sessions 1 and 2 the validator EvnRmnMr (Firedancer) has 0 usable windows in
38 attempted (every send fails at the connection), in all five arms.

## Proposed text

**Supplementary allocation with a cap and a futility rule (outcome-blind).** The allocation
script reads ONLY, per cell (validator x arm; pooled Agave as registered): the registered target
`T` of usable windows, the number of usable windows `U` (§9), the number of attempted windows `A`
(every window of purpose `measure` with at least one reservation in the ledger, whether or not
it is usable), and the cell's planned attempted quota `Q` of the allocation file of session 4. It
takes no tau, p-value, rank, verdict or arm comparison as input.

1. **Cap.** The supplementary attempts of a cell are at most `S = ceil(Q / 2)` (half of the
   cell's planned attempts), over all supplementary sessions together. The quota of a cell is
   `min(S, max(0, T - U))` windows, in attempted windows, as in the allocation files.
2. **Futility.** A cell is declared futile and gets no supplementary windows when `A >= 10` and
   `(T - U) > S x (U / A)`, that is, when even at the cell's own share of usable windows the cap
   `S` cannot close its deficit. With `U = 0` this holds as soon as `A >= 10`. A validator is futile
   in all its cells when it has `U = 0` over all its cells and `A >= 30` summed over them.
3. **What futility does.** Futile cells keep their counts in REPORT.md as "target not reached:
   U of T usable, A attempted"; no value is substituted, no cell is pooled with another to hide the
   gap, and no result of a futile cell is presented as confirmatory evidence of the target power.
   The saved budget stays in the pool of the other cells.
4. **Timing.** The cap and the futility test are evaluated once, after session 4 is finished and
   verified (the registered earliest start of a supplementary session), from the counts at that
   moment, and the result is written to the LOGBOOK with the counts before the supplementary
   allocation file is created. They are not re-evaluated to rescue a cell later.
5. **Date.** This rule is dated 2026-10-28 and committed before sessions 3 and 4 and before any
   supplementary session. The counts it uses are the usable and attempted windows of sessions 1
   to 4 only, which are fixed by the registered allocation files and not by the rule.

Worked example with the counts after session 2 (illustration, nothing is decided): EvnRmnMr has
`U = 0` and `A` of 6 to 8 per arm after two sessions (38 over five arms); at the end of session 4 it
will have `A` of 11 to 16 per arm, so `A >= 10` and `U = 0` hold and every EvnRmnMr cell is
futile: 0 supplementary windows instead of `T` windows per cell.

## Not in this text (separate decision)

- The allocation files of sessions 3 and 4 still contain EvnRmnMr (37 attempted windows left in
  the 4of4 file, about 1.2 million lamports at the file's fee per window). Changing them is a
  deviation from the tagged allocation files, not a use of this rule.
- A contact-address refresh during a session is an engineering change (new binary hash) and is
  independent of this rule.
