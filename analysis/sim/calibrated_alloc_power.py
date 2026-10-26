"""SIMULATION: power of the CALIBRATED allocation (alloc_v2.json, scenario "calibrated") when the
nuisance is the preset low / medium / high level (and the calibrated point as the reference).

The sender plans ATTEMPTED windows (quotas of `plan_quota`). At another nuisance level fewer of
them are usable: usable = attempted x P(>= 5 of 6 land) at that level's p_land. Half-strength power
(the rule only half-applies, the registered 0.9 target) is simulated at the resulting usable count,
per validator as in allocate.py, and for the pooled vanilla Agave cell with the stratified test
(poolpower). BamFiredancer rows are left out (dropped by the 2026-10-26 decision).

Run from analysis/:  python sim/calibrated_alloc_power.py > sim/results/calibrated_alloc_power.txt
"""
import json
import math
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
import numpy as np

from slwlab import allocate, calibrate, poolpower, power, rules, sim

HERE = pathlib.Path(__file__).resolve().parent
REPS = 300
SEED = 20261026
alloc = json.loads((HERE / "results" / "alloc_v2.json").read_text(encoding="utf-8"))["scenarios"]["calibrated"]["allocation"]
rows = [r for r in alloc["rows"] if r["group"] != "BamFiredancer"]
scen = calibrate.scenarios_from_file(str(HERE / "results" / "nuisance_calibrated.json"))
levels = {"preset low": sim.NUISANCE_GRID["low"], "preset medium": sim.NUISANCE_GRID["medium"],
          "preset high": sim.NUISANCE_GRID["high"], "calibrated (reference)": scen["calibrated"]}
rng = np.random.default_rng(SEED)
print(f"calibrated allocation, {len(rows)} rows without BamFiredancer, half-strength power, {REPS} replications, seed {SEED}")
worst = {}
for lname, nz in levels.items():
    urate = power.usable_rate(allocate.N, nz.p_land)
    res = []
    for r in rows:
        fam, arm, rule = r["family"], r["arm"], r["key_rule"]
        names = rules.RULES_BY_ARM[arm]
        w = max(1, math.floor(r["attempted_per_validator"] * urate))
        half = sim.Scheduler("mix", base_sched=allocate.model(fam, arm, rule), p_rule=0.5)
        pl = power.pool(half, arm, allocate.N, nz, REPS, w, rng)
        p_val = float(np.mean([power.holm_normal(x[:w], names).get(rule, 1.0) < power.ALPHA for x in pl]))
        p_pool = None
        if r["group"] == "Agave":  # pooled over the two validators, stratified, 50/50
            pp = power.pool(half, arm, allocate.N, nz, REPS, 2 * w, rng)
            p_pool = float(np.mean([poolpower.reject(poolpower._units(x, [w, w]), names, rule, True) for x in pp]))
        res.append((r["group"], arm, r["attempted_per_validator"], w, p_val, p_pool))
    pv = min(res, key=lambda x: x[4])
    pooled = [x for x in res if x[5] is not None]
    pp = min(pooled, key=lambda x: x[5])
    below = [x for x in res if x[4] < 0.9]
    print(f"\n== {lname}: usable rate {urate:.3f}")
    print(f"  per-validator minimum {pv[4]:.3f} ({pv[0]}/{pv[1]}, attempted {pv[2]}, usable {pv[3]}); rows below 0.90: {len(below)} of {len(res)}")
    print(f"  pooled Agave (stratified) minimum {pp[5]:.3f} (arm {pp[1]}, usable {pp[3]} per validator)")
    for x in sorted(res, key=lambda x: x[4])[:5]:
        print(f"    lowest: {x[0]}/{x[1]} attempted {x[2]} usable {x[3]} power {x[4]:.3f}")
    worst[lname] = {"min_per_validator": pv[4], "min_pooled_agave": pp[5], "rows_below_0.9": len(below), "rows": len(res)}
print("\nSUMMARY " + json.dumps(worst))
