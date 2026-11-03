# Analysis clarifications before the confirmatory run

Written 2026-11-03T12:36Z by the operator's instruction, after tag `data-freeze` (`3ec33cf`) and tag `analysis-pre-run` (`3249c3b`),
and before any confirmatory result was read or computed. They fix details that `docs/PREREGISTRATION.md` leaves open. They change
no registered rule, number or threshold. `PREREGISTRATION.md`, the amendments and the statistics code are untouched.

## a) Positive control (§7)

Registered sentence (§7): "The control passes if the observed mean raw tau lies in the simulator's 95% predictive interval,
taken as the union over the nuisance levels. Provisional, at W = 15: **[0.65, 1.00]** (`alloc_v1.json`, `positive_control_agave_C`).
Re-derived after calibration." And §8: "The positive control (Agave Arm C, §7) is evaluated on the pool."

Fixed:
- **Statistic:** the plain mean (`np.nanmean`, as `analysis/slwlab/allocate.py` `agave_c_control`) over the usable Arm C windows of the
  pooled Agave stratum of the raw tau of `R_RATIO_AGAVE`. From the stored per-validator means this is the window-weighted mean (exact
  unless a window has an undefined tau).
- **Primary interval:** the union of the top-level union (preset low/medium/high) and `scenarios.calibrated.positive_control_agave_C.interval.union`,
  both in `analysis/sim/results/alloc_v2.json`. The preset-only interval and the `scenarios.wifi_sensitivity` one are printed beside it.
  Every value is read from the file.
- **W:** the registered W = 15 interval is used as it is. The achieved number of usable Arm C windows is reported.
- **Pass** iff the observed mean lies inside the primary interval (bounds included). A failed control is reported prominently (§7).
- Script: `scripts/positive_control_check.py` (reads the results json, the pooled json of `slwlab.analyze` and `alloc_v2.json`).

## b) Negative control (§7)

Registered sentences (§7): "**Negative control:** label shuffling within windows must give no significant rule (checked by
the same pipeline). Simulated null false-positive rate: see above." and, above, "Null false-positive rate (any rule, Holm), exact test:
0.02–0.06 by arm."

Fixed:
- **R = 20** shuffles r = 0..19, seed 20261022 + r. Each window gets its own generator, `SeedSequence([20261022 + r, crc32(window_id)])`, so the
  permutation of a window does not depend on the visiting order, and the per-validator and pooled analyses see the same shuffled data.
- **Shuffle:** inside every usable window, the realized landing order of its txs is permuted uniformly at random (the txs swap block
  positions; the set of positions, arrival times, rules, arms and everything else are kept). Unusable windows are left alone.
- **Analysis:** the same functions with the same arguments and defaults as the real run (`analyze.analyze`, `analyze.analyze_pooled` for
  Agave; analysis seed 20261022). The functions are not changed: `parse.windows_from_manifest` is wrapped for the duration of a shuffled
  run and restored.
- **Families and hits:** the Holm families of the pipeline, i.e. (validator, arm) in the per-validator table and (pooled Agave, arm) in the
  pooled table. A family has a hit in a shuffle if one of its rules has a finite Holm-adjusted p below 0.05 (the one-sided alpha of §7).
- **Pass** iff every family has at most 3 hits in the 20 shuffles (null false-positive rate 0.02–0.06 per arm in §7). Counts are reported
  for every family.
- **Time rule:** if shuffle 0 takes longer than 180 s, the pooled cells are dropped for all shuffles and the output says so
  (`pooled_dropped_for_time`).
- Script: `scripts/negative_control.py`. It is not run on the real data until the operator says so.

Note, not a change: with about 40 families a limit of 3 of 20 per family gives roughly 1.6% chance of an exceedance per family under a
true null at 0.05, so one failing family is possible by chance. The result is reported as it is.

## c) Scenario verdict marks (§7 and §6d): NOT implemented

Registered: "a cell whose verdict class differs under the calibrated scenario or the Wi-Fi sensitivity is marked (§6d)" (§0b item 4) and
"Any cell whose verdict class (STRICT, SOFT, NO_RULE, INCONCLUSIVE) differs between the primary and the Wi-Fi sensitivity level is marked in
the verdict table" (§6d). They need new simulator runs at jitter 4.8 ms and per-scenario thresholds that the text does not fix. They will be
reported as specified but not done.

## d) With/without round 2

The sensitivity analysis without session 20261102T123836Z stays as prepared (`scripts/filter_manifest_session.py`, exact commands in
`docs/CONFIRMATORY_RUN_PLAN_FULL.txt`). It is exploratory and not part of the confirmatory verdict.

## Status of the scripts at the time of writing

Both scripts and their tests (synthetic data only) were finished within the time box; neither was run on real data.
