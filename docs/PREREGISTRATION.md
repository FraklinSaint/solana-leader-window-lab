# Pre-registration (DRAFT, not yet frozen)

## 0. What is pre-registered, and when it is frozen

The pre-registered object is the **procedure**: rules, arms, statistics, verdict logic, how
thresholds, allocation and controls are derived from the simulator, and how the simulator's
nuisance parameters are estimated from calibration. The **numbers** in this document are
**provisional**: pre-calibration SIMULATION at preset nuisance levels (`sim_v1.json`,
`alloc_v1.json`, seed 20261022, 200 replications).

Timeline (enforced where code can enforce it):
1. Calibration (≤ 5,000,000 lamports) runs with `--purpose calibrate`. All its data is
   **exploratory** (`data/exploratory/`) and never enters a confirmatory table.
2. `python -m slwlab.calibrate` estimates p_land, clock_sd_ms, offset_ms and bg_between from
   calibration. It uses **order-agnostic statistics only**: landing counts, clock logs, the
   block span of our txs, never their order (tested: shuffling block positions changes no
   estimate). Jitter, execution noise and worker busy time cannot be estimated that way and
   stay at the preset levels. The session RTT record gives an order-agnostic proxy for jitter,
   reported and used only as a labeled sensitivity (§6d).
3. The simulator is re-run with the calibrated values as the new primary level, keeping the
   preset low/high as sensitivity levels. This gives `sim_v2.json` and `alloc_v2.json` (same
   code, same seed). The numbers in §4–§9 are replaced by those results.
4. Any calibrated parameter outside the preset low–high range is flagged by `calibrate.py`
   (`deviation: true`) and appears in REPORT.md as a **labeled deviation**.
5. This document, with the re-derived numbers, is committed and tagged `prereg-v1` **after
   calibration and before the first confirmatory window**. `live --purpose measure` refuses to
   run without `--prereg <commit>` and `--allocation <file>`. After the tag nothing changes.
   Any later deviation is labeled in REPORT.md.

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

**Multi-slot bursts (registered 2026-10-23, before any live window).** At g ≥ 60 ms a burst
spans (N − 1)·g = 300 or 600 ms, about 1.4 or 2.8 slots at ≈ 211 ms. Two of our txs that land in
different slots are ordered by time, not by the scheduler. Therefore, for every gap (and
binding for g ≥ 60 ms):
1. Per window, landed txs are grouped by landed slot. Only pairs within one slot enter tau:
   Kendall tau-b per slot group, combined as the pair-weighted mean. A window with no
   within-slot pair contributes no tau. It is counted and reported.
2. Cross-slot pairs are reported per gap as the **slot-split rate** (share of landed pairs in
   different slots). They are never counted as evidence for or against any rule.
3. The batching granularity estimate above uses within-slot taus only.
4. Power: the simulator gets a slot model. Slot boundaries are at k × slot_ms from the
   window's first slot, with slot_ms the calibrated slot time; a tx gets the slot its
   arrival falls in; scheduling inside a slot is unchanged. Ceilings and power for X at
   g ∈ {60, 120} under rule 1 are computed with `make sim` and recorded in SIMULATION.md before
   `prereg-v1`. The simulated numbers above were computed without a slot model, so they are
   superseded for g ≥ 60 ms. If half-strength power at the allocated W is < 0.9 for a gap, that
   gap is reported as EXPLORATORY and the granularity estimate is restricted to g ≤ 25 ms.

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

## 6c. Independence units (registered 2026-10-23)

Validators whose gossip `tpuQuic` addresses share one IP are one **independence unit**. Our txs
to them reach the same address, so their results are not independent. Verdicts are still
reported per validator. Every statement that counts or pools validators counts units: "k of n
validators follow rule R", agreement within a stratum, any stratum-level pooling (SESSIONS
option B), and any cross-validator interval, where the unit is the resampling unit. The unit is
fixed at plan time from the gossip address recorded in each manifest row (`tx.tpu_quic`) and in
the selection snapshot (`shared_tpu_ips`). For the current selection
(`data/snapshots/shared_tpu_ips_20261022T163227Z.json`), the 10 picked validators form 9 units:
HarmonicAgave `Fd7btgyS` and HarmonicFiredancer `JUPiTERr` share 198.13.140.105 (38 gossip nodes
advertise it). BamAgave `DRpbCBMx` is alone among the picks at 208.91.105.55, which 9 gossip
nodes advertise. What runs at a shared address is not inferred. ASNs are not recorded.

## 6d. Network path and platform strata (registered 2026-10-25, before any live window)

**Registered path: Wi-Fi.** The sending PC has no Ethernet. Smoke, calibration and every
confirmatory window run over Wi-Fi from this one PC. This is a stated limitation (README
"Limitations: Wi-Fi"): the Wi-Fi hop adds delay and jitter that the measurement cannot remove.
It is recorded and reported, never corrected, and it does not exclude windows.

**Record.** Every manifest row carries `host.network_type` (`wifi`, `ethernet`, `vpn`, `other`,
`unknown`). Every session writes `rtt.jsonl` (a timed `getSlot` every 5 s on a kept-alive
connection, first probe of a connection excluded), `wifi.jsonl` (band, channel, signal quality,
RSSI if plausible, PHY, rates, access-point hash, every 30 s) and `network.json` (summary:
RTT min/median/p95/p99/max, robust sigma, jitter = mean absolute difference of consecutive
probes, loss, by UTC hour; link statistics), plus the pre-flight network block. The pre-flight
warns about 2.4 GHz, signal quality below 60 %, adapter power saving, a VPN default route and an
unreadable link. It never refuses a session for the network.

**Platform stratum.** A stratum is (platform id, OS, machine id, network type). Calibration and
all confirmatory windows belong to one stratum. A session on a different network type is a
**different stratum**:
1. `live --purpose calibrate|measure` refuses to start when the network type differs from the
   calibration rows' type (both known; `unknown` is recorded and never refuses);
2. verifier check Q fails if the calibration and confirmatory data contain more than one known
   network type;
3. data from another stratum would need its own calibration, is analyzed separately, and is
   never pooled with this one. Without that, it is EXPLORATORY.

**Noise analysis.** RTT jitter enters the nuisance model as follows. It is a proxy, not the
delay of our QUIC sends to a leader: it also contains the provider's processing and the
internet path, and a burst shares most of its path delay, so the per-tx jitter inside a burst
is smaller than the RTT jitter. `calibrate.py` therefore reports the RTT distribution of the
calibration sessions and the one-way proxy (robust RTT sigma / sqrt 2) beside the preset
`jitter_ms` range, and leaves the primary `jitter_ms` at the preset level. If the proxy exceeds
the preset high level, the simulator is run once more at `jitter_ms` = proxy (the calibrated
level otherwise), and ceilings, thresholds and power at that level are reported as a labeled
**Wi-Fi sensitivity**. Any cell whose verdict class (STRICT, SOFT, NO_RULE, INCONCLUSIVE) differs
between the primary and the Wi-Fi sensitivity level is marked in the verdict table. The per-hour
RTT jitter and signal statistics are reported next to the per-hour clock error.

**Send order realized (registered 2026-10-23).** Each window's actual send order (order of
`start_mono_ns`) must equal its planned `send_rank` (verifier check O). A window where it does
not is excluded with reason `send_order_mismatch`. Before the fix of 2026-10-23, the burst loop
sent zero-gap arms (P, R, C) in idx order, i.e. in ascending price or CU limit. That affected
dry-runs only; no live window was ever sent with it.

## 7. Verdicts (per cell, thresholds from SIMULATION)

Thresholds are looked up for the cell at N = 6 and the largest simulated W ≤ the achieved
number of usable windows (W grid 10/20/30/50). The values quoted here are provisional (§0).
**Strictness on Agave is judged in Arm C.** In Agave arms P and R, STRICT is reported only as
STRICT_RULE_LOW_SEPARATION. Each threshold is the 5th percentile of the
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

**Positive control and strictness test for Agave = Arm C** (vanilla Agave, R_RATIO_AGAVE). The
shared write lock puts the chain on one thread, so interleaving is gone (ceiling ≈ 0.87). The
control passes if the observed mean raw tau lies in the simulator's 95% predictive interval,
taken as the union over the nuisance levels. Provisional, at W = 15: **[0.65, 1.00]**
(`alloc_v1.json`, `positive_control_agave_C`). Re-derived after calibration. A failed positive
control is reported prominently, and it weakens every Agave-family verdict.
**Agave arms P and R are secondary.** Their verdicts carry the low-separability label (§7), and
NO_RULE is structurally unreachable there (README, "What this does NOT separate"). For
reference, the old Arm P interval at W = 20 is [0.19, 0.67].
**Negative control:** label shuffling within windows must give no significant rule (checked by
the same pipeline). Simulated null false-positive rate: see above.

## 8. Sample size, allocation and spend

- **N = 6 txs per window** (kept at the Phase 1 gate). The SIMULATION design rule chose it among
  N ∈ {6, 8, 10}: it gives the best worst-case power for a half-strength rule under the budget.
- **Power-balanced allocation** (`analysis/slwlab/allocate.py`, table in SIMULATION.md):
  - W per (validator group × arm) is the smallest W on the grid 10…80 at which a half-strength
    rule is detected with ≥ 90% power. Arm X W is rounded up to a multiple of 5 (equal counts
    per gap).
  - Required groups (Agave, JitoAgave, Firedancer; 2 validators each) get arms P, X, R, C, F.
    Bonus groups (HarmonicAgave, HarmonicFiredancer, BamAgave, BamFiredancer, Rakurai; 1 each)
    get P, X, C. F is added for bonus groups only if the budget allows.
  - If the total is over budget, bonus cells are trimmed one grid step at a time, most
    expensive first. Required cells are never trimmed.
  - Provisional result: Agave-family P and R need W = 40, C W = 15, X W = 20, F W = 30.
    Firedancer-family cells need W = 15. One bonus cell is trimmed (HarmonicAgave P: W = 30,
    power 0.85). Bonus F does not fit. Confirmatory total ≈ 37.7M lamports.
  - The sender plans by these quotas (`--allocation`, `plan_quota` = attempted windows per
    validator, i.e. W / P(≥ 5 of 6 land)).
- Minimum per validator: ≥ 30 usable windows over ≥ 3 UTC dates.
- **Budgets** (`limits.rs`): calibration ≤ 5,000,000; confirmatory ≤ 38,000,000;
  funding/sweep/retry reserve ≤ 5,000,000; total ≤ 48,000,000 lamports, under the
  50,000,000 hard ceiling. Funding and sweep fees count as spend. Locked rent does not.

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

By send gap (Arm X, §5b), by path (QUIC vs RPC delivery check: landing only), by path quality
(exploratory: windows split at the median of the concurrent RTT jitter and of the signal
quality, §6d), by date, by window
length (4 vs merged 8+ slots), by burst offset, Firedancer `fd4` vs `fd2` ceilings, and
low/high nuisance ceilings (SIMULATION.md).

## 11. Known limits of the simulator (registered)

The models are structural: worker and tile counts and microblock size come from source (F6c),
and pop order from the rules. Background load, execution-time noise, arrival jitter, clock
error and landing probability are not knowable before measurement. The Wi-Fi hop of the
sending PC (§6d) adds delay and jitter that is recorded (RTT, link state) but not removed. They are swept low /
medium / high, and medium is primary. Window-start "prediction error" in our logs is measured
against slot-notification arrival at our machine, not the leader's real block start.

## Source-derived predictions already registered

- VERIFIED_FACTS F6: Agave-family position ≈ R_RATIO order, degraded by parallel-thread
  interleaving; equal-priority ties are NOT in send order.
- VERIFIED_FACTS F6: Firedancer-family follows R_RATIO_FD (exact rational comparison), which gives
  the same permutation as R_RATIO_AGAVE for our arms.
- VERIFIED_FACTS F6b: Arm C strict on both families; Arm F ties: Agave TIE_NONE, Firedancer
  TIE_ARRIVAL_LIFO.
