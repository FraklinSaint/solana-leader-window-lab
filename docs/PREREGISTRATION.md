# Pre-registration (DRAFT, not yet frozen)

This document is frozen, committed and tagged `prereg-v1` **before the first confirmatory live
window**. The sender enforces this: `live --purpose measure` refuses to run without
`--prereg <commit>`. After the tag, thresholds do not change. Any deviation appears in REPORT.md as
a labeled deviation.

Placeholders written **TBD(sim)** are filled from the simulator (Phase 1, label SIMULATION)
before the freeze. No live data can influence them.

## 1. Data classes

| Class | Source | Used for |
|---|---|---|
| Exploratory | dry-runs (`data/logs/dryrun-*`), calibration (`data/exploratory/calibration_manifest.jsonl`), anything before `prereg-v1` | timing, offset choice, landing rates, debugging. **Never** in confirmatory tables. |
| Confirmatory | `data/manifest.jsonl`, written only by `live --purpose measure` on mainnet after the tag | every verdict |
| Supplementary | other transactions in the target blocks | descriptive only, labeled |
| Simulation | `analysis/sim/` | thresholds, power, false-positive rate; labeled SIMULATION everywhere |

## 2. Candidate rules (each predicts a permutation of our N txs)

R_PRICE, R_TOTAL, R_RATIO_AGAVE, R_RATIO_FD (VERIFIED_FACTS F6, transcribed in
`crates/common/src/cost.rs` and `analysis/rules.py`), R_FIFO (send order), R_FBA(T) with
T = 50 ms first, R_RANDOM (null).

## 3. Arms

| Arm | Fee parameters | Send order | Separates |
|---|---|---|---|
| P | geometric price ladder, equal CU limit | seeded random | fee rules vs FIFO/random |
| F | identical price and CU limit | fixed, 10 ms gap | tie-break hypotheses (§4) |
| X | price ascending in send order | fixed, gap swept {0,10,25,60,120} ms | fee vs arrival; batch granularity |
| R | equal price, CU-limit ladder | seeded random | R_PRICE vs R_TOTAL vs R_RATIO |
| C | as P, plus one shared writable account | seeded random | conflict chain (§5), **reported separately** |

P, F, X and R are lock-free: each tx write-locks only its own fee payer (enforced by tests in
`txbuild.rs`). Arms are assigned in balanced, shuffled blocks per validator from the recorded
campaign seed.

## 4. Arm F: competing tie-break predictions

Every Arm F window is scored against each prediction:

| Prediction | Permutation |
|---|---|
| TIE_ARRIVAL_FIFO | send order |
| TIE_ARRIVAL_LIFO | reverse send order (source-derived prediction for Firedancer, F6b) |
| TIE_SIGNATURE | ascending signature bytes |
| TIE_PAYER | ascending payer pubkey bytes |
| TIE_NONE | no observable determinant (source-derived prediction for Agave, F6b) |

**Statement registered in advance:** Agave breaks ties by an internal Slab index that depends on
the whole buffer. From outside, this index **cannot be told apart from random**. So a TIE_NONE
result on Agave is the predicted outcome, not a failure. It is also not evidence that the order is
random inside the validator. TIE_NONE is accepted when no other prediction's mean tau has a CI
excluding 0 (Holm-corrected over the four directional predictions).

## 5. Arm C: conflict chain

Source-derived predictions (F6b): strict priority order on Agave (one thread, batch order) and on
Firedancer (about one tx per microblock, best first). Arm C is analyzed and reported apart from
the lock-free arms. It is not pooled into any per-validator verdict.

## 6. Statistics

- Per window: Kendall tau-b between landed order (block index, same-slot txs only) and each
  rule's prediction; exact permutation p-value (N! orderings, N ≤ 8).
- **Normalized tau.** Even a validator that follows rule R exactly does not reach tau = 1,
  because parallel worker threads interleave independent txs. For each rule R and each client
  family, the simulator gives the distribution of tau under "R + thread interleaving +
  calibrated arrival jitter". Its mean is tau_ceiling(R), TBD(sim). We define
  `tau_norm = tau_obs / tau_ceiling(R)`.
- Per validator × arm: mean tau_norm, window-level bootstrap CI (windows are the unit, 10,000
  resamples), Holm correction across rules, and a paired bootstrap between the top two rules.
- Strictness: share of windows with tau_norm ≥ 0.75, share of perfectly ordered windows, and the
  full distribution.

## 7. Verdicts (on tau_norm)

| Verdict | Condition |
|---|---|
| STRICT_RULE | mean tau_norm ≥ TBD(sim, default 0.9), CI lower bound ≥ TBD(sim, default 0.75), ≥ 90% of windows with tau_norm ≥ 0.75 |
| SOFT_RULE | CI excludes 0, but not STRICT |
| NO_RULE | CI includes 0 **and** the simulator shows ≥ 90% power to detect tau_norm = 0.5 at this validator's window count |
| INCONCLUSIVE | anything else |

**Positive control (vanilla Agave):** passes if the observed mean tau for R_RATIO_AGAVE lies
inside the simulator's 95% predictive interval for "Agave greedy scheduler + interleaving"
(TBD(sim)). A failed positive control is reported prominently, and it weakens every Agave-family
verdict.
**Negative control:** label shuffling within windows. Target false-positive rate on R_RANDOM
≤ 5%, measured on the simulator (TBD(sim)).

## 8. Sample size and spend

- N per window and the number of windows per validator are chosen by simulator power analysis,
  under thread noise and FBA: TBD(sim), with N ∈ {6, 8, 10}.
- Minimum per validator: ≥ 30 usable windows over ≥ 3 UTC dates.
- Planned spend ≤ 35,000,000 lamports (`PLANNED_SPEND_BUDGET_LAMPORTS`). The remaining 15,000,000
  up to the 50,000,000 hard ceiling is reserve. Funding and sweep fees count as spend. Locked
  rent does not.

## 9. Inclusion and exclusion

- **Usable window:** ≥ 6 of 8 txs (scaled with N, TBD) landed in the same slot, produced by the
  intended leader.
- Collector exclusions, recorded per tx in the manifest: `not_landed`, `send_error`,
  `producer_mismatch` (another validator produced the block), `producer_unknown`,
  `outside_window`.
- Every exclusion is reported with its reason and count. **Landing rate per validator is a
  reported result**, not only a filter.

## 10. Sensitivity analyses

By send gap (Arm X), by path (QUIC vs RPC), by date, by window length (4 vs merged 8+ slots), by
burst offset.

## Source-derived predictions already registered

- VERIFIED_FACTS F6: Agave-family position ≈ R_RATIO order, degraded by parallel-thread
  interleaving; equal-priority ties are NOT in send order.
- VERIFIED_FACTS F6: Firedancer-family follows R_RATIO_FD (exact rational comparison), which gives
  the same permutation as R_RATIO_AGAVE for our arms.
- VERIFIED_FACTS F6b: Arm C strict on both families; Arm F ties: Agave TIE_NONE, Firedancer
  TIE_ARRIVAL_LIFO.
