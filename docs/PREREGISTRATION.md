# Pre-registration v1 (text of 2026-10-26; frozen by the git tag `prereg-v1`, which points at the commit holding this text)

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

## 0b. Operator decisions of 2026-10-26 (they take effect with `prereg-v1`)

1. **Frozen simulation set.** (a) The pre-registered preset grid, unchanged: low / medium / high,
   medium primary (p_land 0.90, clock sd 100 ms, offset −100 ms, jitter 1.0 ms). (b) A declared
   **calibrated scenario**: p_land 0.974, clock sd 119.7 ms, bg_between 1.0, offset −300 ms,
   other nuisance at the preset medium (jitter stays at the preset, as step 2 above requires).
   Both are SIMULATION and are frozen together (`sim_v2.json`, `alloc_v2.json`, seed 20261022,
   200 replications; the calibrated one is under `scenarios`). The **Wi-Fi sensitivity**
   (scenario b with jitter 4.8 ms, the RTT proxy, §6d) stays a labeled sensitivity.
2. **Deviations disclosed.** The calibrated p_land (0.974) lies above the preset range
   (0.75–0.90), and the calibrated clock sd (119.7 ms) is above the preset 100 ms. Both are
   `deviation: true` in `nuisance_calibrated.json`. The calibrated offset is a choice, not a
   deviation flag: see 3.
3. **Offset −300 ms.** The calibration sweep was −300, −100, 0, +100 ms. Among offsets with
   ≥ 30 windows, −300 had the best usable rate (0.935; 0: 0.871, +100: 0.867, −100: 0.806).
   The first estimate (−200) was an Arm F artifact (Arm F is sent 200 ms earlier), so it is not
   used. −300 is registered as the burst offset of the calibrated scenario. The preset grid keeps −100. All
   calibration data stays EXPLORATORY.
4. **The allocation the sender reads: the calibrated one (decided 2026-10-26).** Scenario
   `calibrated` of `alloc_v2.json` without BamFiredancer, written as four per-session files with
   cumulative quotas (§8, with their SHA-256). The sender does not read the preset allocation.
   Reasons: it is feasible in 32 hours for `5Us18hLZ` where the preset one is not (§8), and it
   plans 28,053,630 lamports against 37.7M. **Price:** its power assumes the nuisance is near the
   calibrated point. Half-strength power of its cells, simulated at the preset levels
   (`analysis/sim/results/calibrated_alloc_power.txt`, 300 replications, usable windows =
   attempted × the level's usable rate): minimum 0.66 at preset low, **0.63 at preset medium** (25
   of 31 cells below 0.90) and **0.24 at preset high** (all 31 below); the pooled Agave cells
   (stratified) 0.91 at low and medium and 0.48 at high. At the calibrated point itself the
   minimum is 0.89 (2 cells at 0.89–0.90). Verdict thresholds and ceilings are still read at the
   preset medium level (§7); a cell whose verdict class differs under the calibrated scenario or
   the Wi-Fi sensitivity is marked (§6d). Outcome-blind supplementary sessions (§8) are the
   remedy for fewer usable windows than registered; they do not restore power that a worse
   nuisance level would have needed.
5. **Option B** (§8), **BamFiredancer dropped** (§8, §12), **gap rules** (§5b) and the
   **skipped-slot rule** (§9) are decided and written below.

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
| X | price ascending in send order | fixed, gap swept {0, 10, 25, 60} ms (confirmatory) and 120 ms (EXPLORATORY only, §5b) | fee vs arrival; batching granularity (§5b) |
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

**Confirmatory and exploratory gaps (decided 2026-10-26).** Gaps up to 60 ms (0, 10, 25, 60)
are confirmatory. The 120 ms gap is **EXPLORATORY only**, whatever its power: at 120 ms a burst
of N = 6 spans 600 ms, about 2.8 slots, so many pairs land in different slots and fewer
within-slot pairs remain. Its windows are still sent and reported (gap response, slot-split rate), but they
enter no verdict and no granularity estimate. The granularity estimate uses g ≤ 60 ms.

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
    Bonus groups (HarmonicAgave, HarmonicFiredancer, BamAgave, Rakurai; 1 each) get P, X, C.
    F is added for bonus groups only if the budget allows. **BamFiredancer is dropped** (§12):
    the selection snapshot has no such validator, and its quota cannot be filled.
  - If the total is over budget, bonus cells are trimmed one grid step at a time, most
    expensive first. Required cells are never trimmed.
  - *Registered allocation* (decided 2026-10-26, §0b item 4): scenario `calibrated` of
    `alloc_v2.json`, 886 attempted windows, 28,053,630 lamports of the 38,000,000 (BamFiredancer's
    2,004,384 left out; spare 9,946,370). Per validator and arm, attempted windows: Agave and
    JitoAgave P 26, X 21, R 21, C 16, F 21; Firedancer P/X/C 16, R 11, F 16 (Firedancer-family
    cells need W = 15, R W = 10); bonus groups P, X, C, F as in `alloc_v2.json`
    (`scenarios.calibrated`). The preset
    allocation (P 40, C 15, X 20, F 30, 37.7M) is kept as a reference result only.
  - The sender plans by these quotas (`--allocation`, `plan_quota` = attempted windows per
    validator, i.e. W / P(≥ 5 of 6 land)).
- **Option B: vanilla Agave is pooled at analysis level (decided 2026-10-26; `docs/SESSIONS.md`).**
  The sender is unchanged: quotas stay per validator (`plan_quota["Agave"]` is the same for the
  two picked vanilla Agave validators, `7QQGNm3p` and `5Us18hLZ`). The pool is those two
  validators; the pooled claim, the stratified test and the descriptive per-validator tables are
  made by the analysis. The pool's target is 2 × the per-validator quota. All other strata are
  as in option A.
  - **The pooled test is stratified by independence unit** (§6c), not a sum over all windows.
    The sum of window taus of §6 would let the unit with the most windows decide the verdict.
    Code: `stats.stratified_randomization_pvalue`, `analyze.analyze_pooled`.
    1. *Units and estimate.* Per cell (arm × rule) each unit's estimate is the mean tau of its
       usable windows. The pooled statistic is the mean of the unit means over the units that
       have ≥ 5 usable windows in that cell, each with weight 1/K. A unit with fewer windows is
       descriptive only and is listed. With fewer than 2 such units there is no pooled test and
       the cell is INCONCLUSIVE.
    2. *Permutation.* One-sided randomization test (H1: the rule predicts the order). In every
       window of every unit the order is redrawn from that window's own exact null (§6), so
       the permutation is inside each window (finer than inside each validator), and the
       statistic is recomputed with the same unit weights. p = (1 + #null ≥ observed) / (1 +
       20,000). Holm across the rules of the arm.
    3. *Interval and strictness.* Stratified percentile bootstrap (windows resampled inside
       each unit, same weights); tau_norm and the share of windows with tau_norm ≥ 0.5 use the
       same weights. STRICT thresholds and the half-strength power are SIMULATED at the actual
       usable windows per unit (`poolpower.cell_reference`), not read from the single-stream
       W grid of §7.
    4. *What changed in the simulation* (`pool_v2.json`, Agave cells only, 200 replications,
       SIMULATION, preset medium and the calibrated scenario): with the planned pool sizes and
       balanced units (60/40; the plan's equal quotas give about 50/50), stratified
       half-strength power is 0.98–1.00, the same as for the sum (0.98–1.00). The false-positive
       rate stays at or below 0.055 in every split (200 replications, so ±0.015). What it
       costs: units with < 5 windows in a cell are dropped. With a 90/10 split the second unit
       has 3–8 windows: at 3–4 windows the cell has no pooled test (power 0.00, Arms C, R and
       X), at 5–8 windows half-strength power falls to 0.79–0.88 (Arms P and R). The planner
       should therefore keep each pooled validator at ≥ 5 windows per cell or cap its share.
       **Stratifying
       limits the weight of a unit; it does not test that the units agree.** If one unit follows
       a rule fully and the other is random, the pooled test still rejects (simulated: 0.99–1.00).
       Heterogeneity is shown, not tested: the per-unit tau_norm table is reported next to the
       pooled verdict.
  - The stratum verdict (§7) is a claim about the vanilla Agave stratum and holds only if the
    pooled validators follow the same rule. Pooled power is not comparable to a per-validator
    power.
  - **Per-validator results inside the pool are DESCRIPTIVE ONLY**: mean tau, tau_norm and
    landing rate per validator and arm, with the window count, and no verdict, no p-value
    claim and no power claim at validator level.
  - The positive control (Agave Arm C, §7) is evaluated on the pool.
  - *Planner and feasibility (no planner change).* The planner reads one quota per stratum and
    applies it to every validator of that stratum (`planner.rs`), so unequal quotas inside the
    stratum cannot be expressed. They are not needed. Simulated for 32 h (4 × 8 h, 15% or 30%
    of windows lost, 5 s spacing, global spend pacing, 20 seeds, SIMULATION,
    `analysis/sim/feasibility_32h.py`): with the
    calibrated allocation (per validator P 26, X 21, R 21, C 16, F 21 = 105) every quota of the
    plan is met in 20/20 runs. Over 32 h `5Us18hLZ` has 357 leader windows and can supply about
    142 (15% lost) or 124 (30% lost) attempted windows under pacing, which is 35% or 18% slack
    over 105; `7QQGNm3p` has 573 and supplies 222 or 187. A 25% margin on the Agave quotas
    (131 per validator) is met at 15% lost (19/20 runs) and not at 30% (1/20). With the preset
    allocation (166 per validator), `5Us18hLZ` can supply only 99–151, so its quota is not met.
  - *Spreading over sessions.* The planner has no per-session quota; left alone, `7QQGNm3p`
    would fill its quota in the first one or two sessions. Each session therefore gets its own
    allocation file with cumulative quotas (see "Per-session allocation files" below); no
    planner change is needed.
- Minimum per validator: ≥ 30 usable windows over ≥ 3 UTC dates (all arms together). It also
  holds for each validator inside the pooled Agave stratum, where each tested cell in addition
  needs ≥ 5 usable windows per unit (§8, pooled test).
- **Per-session allocation files.** The planner has no per-session quota, so each session gets
  a file with cumulative quotas, `ceil(quota × k / 4)` per stratum and arm, written by
  `python -m slwlab.session_alloc` (deterministic, LF line ends; `.gitattributes` keeps them
  unconverted). `runner::prior_plan_state` counts the windows attempted in earlier sessions from
  the spend ledger and each run's `plan.jsonl`, so what a session leaves unfilled is asked again
  by the next file, and the last file holds the full quota. `--allocation` names the file; the
  sender records the path and `--prereg` string as given in `session.json`, it does not hash the
  file or check the commit, so the hashes below and the pre-start check of RUNBOOK_MAINNET S7b
  are the check.

  | Session | File (`analysis/sim/results/sessions/`) | SHA-256 |
  |---|---|---|
  | 1 | `alloc_calibrated_session1of4.json` | `50e5dc55895929a0618f8fc3bfebff1402ce82d7892b34997b4456e4b408f5af` |
  | 2 | `alloc_calibrated_session2of4.json` | `4e2b2c4934d4c769f1fd3ae70c9b419853c8ea4339855de324f77585ffc6adff` |
  | 3 | `alloc_calibrated_session3of4.json` | `7dbdab5def178f34d4c96a09a31c76f2f812349fee2f19cd6046d8796792d1f3` |
  | 4 | `alloc_calibrated_session4of4.json` | `d6a663ac94bd5dd76afd46a2d0a242f0447014a378077a281c6217e1c31959e8` |

  Cumulative planned cost: 7,569,190 / 14,346,268 / 21,344,952 / 28,053,630 lamports. Each
  session is paced along a straight line from 0 to its cap over its hours (planner `Pace`), so the
  cap must match what the session has to do, or the quota fills early and the last hours stay idle
  (a cap of 9,500,000 for a quarter of about 7.6M would). The cap is computed by
  `scripts/session_cmd.py` from the allocation file and the ledger: `ceil((cumulative quota of the
  session's file − windows already attempted) × fee per window × 1.05 × H / 8)`, H = the session's
  hours. Earlier shortfalls are inside it, a shorter session gets a proportionally smaller cap.
  Session 1 with H = 8: 239 windows, 7,569,190 lamports, cap 7,947,650 (a pacing line of 993,456
  lamports an hour, about 31 windows an hour; the quota is used up after about 7.6 h).
- **Supplementary sessions (outcome-blind).** After each session, and after the fourth, usable
  windows (§9) and UTC dates are counted per validator and arm. A cell is in deficit if it has
  fewer usable windows than its registered target (the `W_usable_per_validator` of the
  allocation, summed over the pool for pooled Agave), or a validator has fewer than 30 usable
  windows or fewer than 3 UTC dates once the planned sessions are done. While a deficit exists
  and confirmatory budget remains, a supplementary session runs on a spare day with the same
  mechanism: the same binary, `--mode measure`, a paced cap from what is left, and an allocation
  file whose cumulative quota for the deficient cells is raised by deficit / 0.991 (the
  registered usable rate), rounded up. It stops when every cell is filled or the 38,000,000 are
  used. **The decision uses only counts of usable windows and dates.** It never uses tau,
  p-values, ranks, verdicts or which rule wins. Budget: the spare 9,946,370 buys about 314 more
  windows (35% of the 886 planned), enough if the usable rate turns out as low as about 0.73
  (planned 0.99; preset medium 0.89), not at the preset-high 0.53. [OPEN: the script that writes
  the supplementary allocation file from the counts is not written; it is not needed before the
  planned sessions are done.]
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
- **Skipped slots (decided 2026-10-26).** A window with any of its slots skipped (4 slots for a
  normal window; all slots of a merged window) is **excluded**, with reason `skipped_slot`.
  The decision comes from the chain's own record of which slots have blocks: `getBlocks`
  (finalized) over the window's slot range, fetched after the window. A slot missing from that
  list is skipped. It does **not** come from our transactions (whether they landed, or where).
  Otherwise the exclusion would depend on the outcome we measure.
  - *Where it lives.* The collector and the sender binary are unchanged (the collector saves
    only blocks that hold our txs, so it cannot see an empty slot). After a run, once its last
    window is finalized, `python -m slwlab.blocklist fetch` saves
    `data/logs/<run_id>/blocklist.json` (the slots with blocks per window, the `getBlocks` calls,
    the finalized slot at fetch time). `parse.windows_from_manifest` classifies from it. It is
    never guessed: `python -m slwlab.analyze` refuses to run on a manifest with a confirmatory
    run that has no list (message names the run and the command), and at library level such a
    window is excluded as `slot_status_missing`. The provider served `getBlocks` and `getBlock`
    200 days back when probed (2026-10-26), so there is no hard deadline; the runbook (S8 step 2)
    has the fetch run the same day, after `collect` and before `reconcile` and `verify`. First
    run on mainnet data: the smoke and the calibration (156 windows, 0 skipped slots, check K ok).
  - *Verifier.* `scripts/verify_manifest.py` check K fails when a confirmatory run has no list, or
    a list is not finalized, does not cover a window's slots, or contradicts the blocks we hold or
    the slots our txs landed in (it only catches a wrong list; it never derives one). `--online`
    re-fetches the lists.
  - *Reporting.* Counted and reported **per validator** (windows with a skipped slot, windows
    total, share) next to the landing rate (`skipped_slots_by_validator`). A **sensitivity
    analysis** (`analyze --include-skipped`) repeats the verdict table with these windows
    included (their usable txs, same-slot pairs only, §5b) and labels any cell whose verdict
    class changes.
  - *Status.* Implemented, unit-tested on synthetic data, and run once on the two calibration
    runs (read-only RPC). The comparison of the two verdict tables (with and without these windows) is done by running
    `analyze` twice and comparing; there is no automatic label yet.

## 10. Sensitivity analyses

With windows that have a skipped slot included (§9), by send gap (Arm X, §5b), by path (QUIC vs RPC delivery check: landing only), by path quality
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

## 12. Registered limitations (2026-10-26)

- **BamFiredancer is not measured.** The selection snapshot (`selection_20261022T163227Z.json`)
  has no BamFiredancer validator, so the group is dropped from the allocation. This study makes
  no statement about BAM-scheduled Firedancer. Nothing from BamAgave or Firedancer is
  extrapolated to it.
- **Pooled Agave.** Per-validator Agave results are descriptive; the Agave verdict is a
  stratum-level claim (§8).
- **Groups with one validator** (HarmonicAgave, HarmonicFiredancer, BamAgave, Rakurai): a
  finding is about that one validator (its software build, configuration, stake, network
  position and the path from our PC to it). It is not a finding about the client family, the
  scheduler type or the product name, and it is not extrapolated to other validators of that
  group.
- **The DRpbCBMx pattern** (BamAgave). In calibration, 3 of the 4 windows in which no tx landed
  were at `DRpbCBMx`, and 18 of its 138 txs (13%) were lost, against 6 of 120 at JitoAgave
  `E1r4Psq8` (1 window) and none at the other 8 validators (LOGBOOK 2026-10-25 21:55). All 16
  slots of those four windows produced blocks, so skipped slots do not explain it. This is
  reported as a result about that validator. It is not extrapolated to other BAM validators,
  and it says nothing about BAM as a class until more than one BAM validator is measured.
- **Calibration coverage.** The calibration windows fell into a span of about 23 minutes
  (15:56–16:19 UTC on Sunday 2026-10-25), although the session ran 3 hours, because spend was
  not paced over the session (fixed by the pacing rule of 2026-10-25). One Sunday evening is
  one time of day and one day of the week. Calibrated nuisance values (p_land, clock sd,
  offset) are not known to hold at other hours; they are EXPLORATORY inputs to a labeled
  scenario, and the preset grid (§0b) stays the primary level.
- **One PC, one Wi-Fi path, one stratum** (§6d), as registered.
- **Calibrated scenario.** It is a SIMULATION built from 936 calibration txs (EXPLORATORY), with
  deviations disclosed in §0b.

## Source-derived predictions already registered

- VERIFIED_FACTS F6: Agave-family position ≈ R_RATIO order, degraded by parallel-thread
  interleaving; equal-priority ties are NOT in send order.
- VERIFIED_FACTS F6: Firedancer-family follows R_RATIO_FD (exact rational comparison), which gives
  the same permutation as R_RATIO_AGAVE for our arms.
- VERIFIED_FACTS F6b: Arm C strict on both families; Arm F ties: Agave TIE_NONE, Firedancer
  TIE_ARRIVAL_LIFO.
