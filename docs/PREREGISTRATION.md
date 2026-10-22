# Pre-registration (DRAFT, not yet frozen)

This document is frozen, committed and tagged `prereg-v1` **before the first confirmatory live
window**. The sender enforces this: `live --purpose measure` refuses to run without
`--prereg <commit>`. After the tag, thresholds do not change. Any deviation appears in REPORT.md as
a labeled deviation.

Every number below that was `TBD(sim)` now comes from the simulator: `analysis/slwlab/sim.py`
and `power.py`, seed 20261022, 200 replications per cell. The results are in
`analysis/sim/results/sim_v1.json`, rendered as `docs/SIMULATION.md` (`make sim` reproduces
both). They are labeled **SIMULATION**. No measurement influenced them: no live window exists.
The frozen copy of `sim_v1.json` at the `prereg-v1` commit is the reference. If the code
changes later, these values do not.

## 1. Data classes

| Class | Source | Used for |
|---|---|---|
| Exploratory | dry-runs (`data/logs/dryrun-*`), calibration (`data/exploratory/calibration_manifest.jsonl`), anything before `prereg-v1` | timing, offset choice, landing rates, debugging. **Never** in confirmatory tables. |
| Confirmatory | `data/manifest.jsonl`, written only by `live --purpose measure` on mainnet after the tag | every verdict |
| Supplementary | other transactions in the target blocks | descriptive only, labeled |
| Simulation | `analysis/sim/` | ceilings, thresholds, power, false-positive rate; labeled SIMULATION everywhere |
| Delivery check | manifest lines with `ordering_eligible: false` (path `rpc-helius-staked`, one tx per window) | landing rate only, reported separately; **never** in ordering analysis |

## 2. Candidate rules (each predicts a ranking of our N txs; ties allowed)

R_PRICE, R_TOTAL, R_RATIO_AGAVE, R_RATIO_FD (VERIFIED_FACTS F6, transcribed in
`crates/common/src/cost.rs` and `analysis/slwlab/model.py`, cross-tested), R_FIFO (actual send
order), R_FBA50 (50 ms arrival buffers, FIFO across buffers, priority fee inside a buffer),
R_RANDOM (null). Implementation: `analysis/slwlab/rules.py`.

## 3. Arms

| Arm | Fee parameters | Send order | Separates |
|---|---|---|---|
| P | geometric price ladder 10k–1M µL/CU, CU limit 1000 | seeded random, no gap | fee rules vs random |
| F | identical price (50k) and CU limit | fixed, 10 ms gap, sent **200 ms earlier** than other arms | tie-break hypotheses (§4) |
| X | price ascending in send order | fixed, gap swept {0, 10, 25, 60, 120} ms | fee vs arrival; batching granularity (§5b) |
| R | equal price 200k, CU-limit ladder 1000…6000 | seeded random, no gap | R_PRICE vs R_TOTAL vs R_RATIO |
| C | as P, plus one shared writable account | seeded random, no gap | conflict chain (§5), **reported separately** |

P, F, X and R are lock-free: each tx write-locks only its own fee payer (tests in `txbuild.rs`).
Arms are assigned in balanced, shuffled blocks per validator from the recorded campaign seed.

**Arm F extra lead (registered from SIMULATION).** A 10 ms-gapped burst spans 50 ms at N = 6.
If it straddles the window start, the late txs fall into later scheduling passes and look
FIFO. That would fake a tie-break determinant on Agave. With a 100 ms clock error this hit
~25% of simulated windows. Sending Arm F 200 ms earlier (`ArmConfig.f_extra_lead_ms`) brings it
to ≈ 1% (normal approximation: P(−300 + 50 + N(0, 100) > 0)).

## 4. Arm F: competing tie-break predictions

Every Arm F window is scored against each prediction:

| Prediction | Ranking |
|---|---|
| TIE_ARRIVAL_FIFO | send order |
| TIE_ARRIVAL_LIFO | reverse send order (source-derived prediction for Firedancer, F6b) |
| TIE_SIGNATURE | ascending signature bytes |
| TIE_PAYER | ascending payer pubkey bytes |
| TIE_NONE | no observable determinant (source-derived prediction for Agave, F6b) |

**Statement registered in advance:** Agave breaks ties by an internal Slab index that depends on
the whole buffer. From outside, this index **cannot be told apart from random**. So a TIE_NONE
result on Agave is the predicted outcome, not a failure. It is also not evidence that the order is
random inside the validator. TIE_NONE is accepted when none of the four directional predictions
is significant after Holm (one-sided α = 0.05).
SIMULATION, N = 6: the Firedancer model's LIFO is detected with power 1.00 already at W = 10.
Under the Agave model, each tie rule comes out significant in ≤ 4% of replications.

## 5. Arm C: conflict chain

Source-derived predictions (F6b): strict priority order on Agave (one thread, batch order) and on
Firedancer (one tx per microblock, best first). Arm C is analyzed and reported apart from
the lock-free arms. It is not pooled into any per-validator verdict.
SIMULATION: the ceiling is ≈ 0.86–0.90 on both families. Interleaving is gone, and what remains
comes from bursts that arrive after the window start. Arm C is where STRICT is cleanly
separable on Agave (false-STRICT on a half-strength rule: 0.00).

## 5b. Arm X: gap response (registered from SIMULATION)

Per validator and per gap g, report the mean tau of R_PRICE and R_FIFO with bootstrap CIs. The
**batching granularity estimate** is the smallest g whose R_PRICE CI upper bound is below 0.
Simulated signatures (N = 8, mean tau R_PRICE at gap 0/10/25/60/120 ms): Agave
0.43/0.00/−0.37/−0.85/−0.96; Firedancer 0.82/0.32/−0.25/−0.82/−0.96; FBA(50 ms)
0.97/−0.13/−0.74/−1.00/−1.00; FIFO −0.02/−1.00/−1.00/−1.00/−1.00.

## 6. Statistics

- Per window: Kendall tau-b between the landed order (block index, same-slot txs only) and each
  rule's ranking.
- **Test:** a randomization test of the sum of window taus, each window drawn from its own
  exact null (all orderings of its landed txs), one-sided (H1: the rule predicts the order).
  Holm correction across the rules of the arm. `stats.randomization_pvalue`.
- **Cells and ceilings.** A cell is (client family, arm, rule). Its model is "rule R decides
  the pop order + that family's thread/microblock interleaving + medium nuisance".
  `tau_ceiling` is the mean tau of R under that model, and `tau_norm = tau_obs / tau_ceiling`.
  Family by stratum: Agave, JitoAgave, HarmonicAgave, BamAgave, Rakurai → `agave`
  (4 workers); Firedancer, HarmonicFiredancer, FireBam → `fd2` (2 exec tiles; `fd4` is a
  sensitivity check). Ceilings: SIMULATION.md "tau ceilings".
- **Identifiability:** a cell with ceiling < **0.3** is not identifiable. Its tau is reported
  raw and the rule can never be STRICT there (e.g. R_FIFO in arms P, R, C, which have no send
  gap).
- Per validator × arm: mean tau and tau_norm, window-level percentile bootstrap CI (10,000
  resamples), Holm-adjusted p, and a paired bootstrap between the top two rules.
- Strictness: share of windows with tau_norm ≥ 0.5, share of perfectly ordered windows, and the
  full distribution.

## 7. Verdicts (per cell, thresholds from SIMULATION)

Thresholds are looked up for the cell at N = 6 and the largest simulated W ≤ the achieved
number of usable windows (W grid 10/20/30/50). Each threshold is the 5th percentile of the
statistic under the cell's exact model. Example: Agave, Arm P, R_RATIO_AGAVE, W = 20:
mean tau_norm ≥ 0.80, CI lower ≥ 0.33, share ≥ 0.55. All cells: SIMULATION.md.

| Verdict | Condition |
|---|---|
| STRICT_RULE | rule significant (Holm, one-sided α = 0.05) **and** the cell is identifiable **and** mean tau_norm, CI lower bound and window share all meet the cell's thresholds |
| STRICT_RULE_LOW_SEPARATION | as STRICT, but in that cell the simulator calls a half-strength rule STRICT > 10% of the time at that W (Agave arms P and R at W = 20: 11–17%; Agave Arm X with R_FBA50: 20%) |
| SOFT_RULE | significant, but not STRICT |
| NO_RULE | not significant **and** the simulator's power for a half-strength version of the rule (tau_norm ≈ 0.5) is ≥ 90% at this W |
| INCONCLUSIVE | anything else |

SIMULATION at N = 6, W = 20 (medium nuisance):
- Exact rules are detected with power 1.00 in every identifiable main cell.
- Half-strength rules: 0.94–0.98 on Firedancer (P, R, C) and Agave C, but only 0.56–0.72 on
  Agave P and R. NO_RULE is therefore not reachable for Agave lock-free arms at W = 20.
- Null false-positive rate (any rule, Holm), exact test: 0.02–0.06 by arm.

**Positive control (vanilla Agave, R_RATIO_AGAVE, Arm P):** passes if the observed mean raw tau
lies in the simulator's 95% predictive interval, taken as the union over the nuisance levels.
At N = 6, W = 20 that interval is **[0.19, 0.67]**. Other W: `positive_control` in `sim_v1.json`.
A failed positive control is reported prominently, and it weakens every Agave-family verdict.
**Negative control:** label shuffling within windows must give no significant rule (checked by
the same pipeline). Simulated null false-positive rate: see above.

## 8. Sample size and spend

- **N = 6 txs per window.** The SIMULATION design rule picks, among N ∈ {6, 8, 10}, the N whose
  budget-affordable W gives the highest worst-case power for a half-strength rule over the
  main cells, provided the exact rule has ≥ 95% power. N = 6: 0.66, N = 8: 0.47, N = 10: 0.48.
- **Target W = 20 usable windows per validator × arm.** Layout: 40 cells = 3 required strata ×
  2 validators × 5 arms, plus 5 upside validators × arms P and C.
- Minimum per validator: ≥ 30 usable windows over ≥ 3 UTC dates.
- Planned spend ≤ 35,000,000 lamports (`PLANNED_SPEND_BUDGET_LAMPORTS`). At N = 6 a window costs
  31,895 lamports on average when every tx lands, so 90% of the budget buys about 987 windows.
  The remaining 15,000,000 up to the 50,000,000 hard ceiling is reserve. Funding and sweep fees
  count as spend. Locked rent does not.

## 9. Inclusion and exclusion

- **Usable window:** ≥ 5 of 6 txs (ceil(0.75 N)) landed in the same slot, produced by the
  intended leader, with no collector exclusion, and `ordering_eligible` for every tx.
- Collector exclusions, recorded per tx in the manifest: `not_landed`, `send_error`,
  `producer_mismatch` (another validator produced the block), `producer_unknown`,
  `outside_window`.
- Every exclusion is reported with its reason and count. **Landing rate per validator is a
  reported result**, not only a filter. The `rpc-helius-staked` delivery check (one tx per
  window) is reported as a landing comparison with QUIC only.

## 10. Sensitivity analyses

By send gap (Arm X, §5b), by path (QUIC vs RPC delivery check: landing only), by date, by window
length (4 vs merged 8+ slots), by burst offset, Firedancer `fd4` vs `fd2` ceilings, and
low/high nuisance ceilings (SIMULATION.md).

## 11. Known limits of the simulator (registered)

The models are structural: worker and tile counts and microblock size come from source (F6c),
and pop order from the rules. Background load, execution-time noise, arrival jitter, clock
error and landing probability are not knowable before measurement. They are swept low /
medium / high, and medium is primary. Window-start "prediction error" in our logs is measured
against slot-notification arrival at our machine, not the leader's real block start.

## Source-derived predictions already registered

- VERIFIED_FACTS F6: Agave-family position ≈ R_RATIO order, degraded by parallel-thread
  interleaving; equal-priority ties are NOT in send order.
- VERIFIED_FACTS F6: Firedancer-family follows R_RATIO_FD (exact rational comparison), which gives
  the same permutation as R_RATIO_AGAVE for our arms.
- VERIFIED_FACTS F6b: Arm C strict on both families; Arm F ties: Agave TIE_NONE, Firedancer
  TIE_ARRIVAL_LIFO.
