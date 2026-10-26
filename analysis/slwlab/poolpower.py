"""Power of the pooled, stratified Agave test (PREREGISTRATION §8, option B). SIMULATION.

``python -m slwlab.poolpower --out sim/results/pool_v2.json``

Reruns only the Agave-family cells of ``power.py`` for the pooled design: the pool's W usable
windows (from ``alloc_v2.json``, the Agave group) are split over units (validators, or
independence units) and tested two ways: the window-weighted sum of taus (the pooled test
before 2026-10-26) and the stratified mean (``stats.stratified_randomization_pvalue``; here the
normal approximation under each window's exact null variance, as in ``power.holm_normal``).
Nothing in ``sim_v2.json`` / ``alloc_v2.json`` is changed. Same seed scheme, 200 replications.
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import time

import numpy as np

from . import calibrate, power, rules, sim, stats

ALPHA = power.ALPHA
MIN_N = stats.MIN_UNIT_WINDOWS
N = 6
# usable-window shares over pooled units: two picked validators 60/40, six-validator pool in
# proportion to the saved leader schedule (windows/h of the six vanilla Agave candidates), and
# a stress case with one dominant unit.
SPLITS = {
    "2 units 60/40": [0.6, 0.4],
    "6 units by leader rate": [17.91, 11.16, 3.20, 2.74, 1.61, 1.22],
    "2 units 90/10": [0.9, 0.1],
}


def split_counts(total: int, shares: list[float]) -> list[int]:
    s = np.asarray(shares, float)
    s = s / s.sum()
    raw = s * total
    c = np.floor(raw).astype(int)
    for i in np.argsort(-(raw - c))[: total - c.sum()]:
        c[i] += 1
    return [int(x) for x in c]


def _units(wins: list[dict], counts: list[int]) -> list[list[dict]]:
    out, i = [], 0
    for c in counts:
        out.append(wins[i:i + c])
        i += c
    return out


def p_window_weighted(wins: list[dict], r: str) -> float:
    t = [x[r][0] for x in wins if not math.isnan(x[r][0])]
    v = [x[r][1] for x in wins if not math.isnan(x[r][0])]
    return 0.5 * math.erfc(sum(t) / math.sqrt(sum(v)) / math.sqrt(2)) if t and sum(v) > 0 else float("nan")


def p_stratified(units: list[list[dict]], r: str) -> float:
    ts = [[x[r][0] for x in u if not math.isnan(x[r][0])] for u in units]
    vs = [[x[r][1] for x in u if not math.isnan(x[r][0])] for u in units]
    keep = stats.stratified_units(ts, MIN_N)
    if len(keep) < 2:
        return float("nan")
    k = len(keep)
    stat = sum(np.mean(ts[i]) for i in keep) / k
    var = sum(sum(vs[i]) / len(ts[i]) ** 2 for i in keep) / k ** 2
    return 0.5 * math.erfc(stat / math.sqrt(var) / math.sqrt(2)) if var > 0 else float("nan")


def holm_all(units: list[list[dict]], names: list[str], stratified: bool) -> dict[str, float]:
    allw = [w for u in units for w in u]
    p = {r: (p_stratified(units, r) if stratified else p_window_weighted(allw, r)) for r in names}
    return stats.holm(p)


def reject(units: list[list[dict]], names: list[str], r: str, stratified: bool) -> bool:
    v = holm_all(units, names, stratified).get(r, float("nan"))
    return (not math.isnan(v)) and v < ALPHA


def strat_cell_stats(units_per_rep: list[list[list[dict]]], names: list[str], rule: str, ceiling: float,
                     rng: np.random.Generator, boot: int = 500) -> dict:
    sig, means, lows, shares = 0, [], [], []
    for units in units_per_rep:
        if reject(units, names, rule, True):
            sig += 1
        tn = [np.asarray([x[rule][0] for x in u], float) / ceiling for u in units]
        tn = [t[~np.isnan(t)] for t in tn]
        m = stats.stratified_mean(tn, MIN_N)
        means.append(m)
        lows.append(stats.stratified_bootstrap_ci(tn, rng, boot)[0])
        keep = stats.stratified_units(tn, MIN_N)
        shares.append(float(np.mean([np.mean(tn[k] >= power.WINDOW_TAU_NORM) for k in keep])) if len(keep) >= 2 else float("nan"))
    return {"power": sig / len(units_per_rep), "_m": means, "_l": lows, "_s": shares}


def cell_reference(fam: str, arm: str, rule: str, counts: list[int], nz: sim.Nuisance, ceiling: float,
                   reps: int, rng: np.random.Generator) -> dict:
    """Simulated reference for one pooled cell at the ACTUAL usable-window counts per unit:
    stratified power (exact and half-strength rule) and the STRICT thresholds (5th percentile
    of the stratified statistics under the exact model). Used by `analyze.analyze_pooled`."""
    names = rules.RULES_BY_ARM[arm]
    w = sum(counts)
    model = power.model_for(fam, rule)
    full = [_units(x, counts) for x in power.pool(model, arm, N, nz, reps, w, rng)]
    half = [_units(x, counts) for x in
            power.pool(sim.Scheduler("mix", base_sched=model, p_rule=0.5), arm, N, nz, reps, w, rng)]
    sf = strat_cell_stats(full, names, rule, ceiling, rng)
    sh = strat_cell_stats(half, names, rule, ceiling, rng)
    th = {"alpha": ALPHA, "window_tau_norm": power.WINDOW_TAU_NORM,
          "strict_mean": _q(sf["_m"], 0.05), "strict_ci_lower": _q(sf["_l"], 0.05),
          "strict_window_share": _q(sf["_s"], 0.05)}
    return {"counts": counts, "power_exact": sf["power"], "power_half": sh["power"], "thresholds": th,
            "p_strict_half": power.strict_rate(sh, th)}


def _q(a, q):
    a = np.asarray(a, float)
    a = a[~np.isnan(a)]
    return round(float(np.quantile(a, q)), 2) if len(a) else None


def run_level(level: str, nz: sim.Nuisance, ceilings: dict, w_pool: dict[str, int], reps: int, seed: int, log) -> dict:
    rng = np.random.default_rng(seed)
    res: dict = {"nuisance": level, "w_pool": w_pool, "cells": {}, "fpr": {}, "dominance": {}}
    t0 = time.time()
    for fam, arm, rule in power.cells():
        if fam != "agave" or arm not in w_pool:
            continue
        ceil = ceilings.get(arm, {}).get(rule)
        if ceil is None or ceil < power.MIN_CEILING:
            continue
        names = rules.RULES_BY_ARM[arm]
        w = w_pool[arm]
        model = power.model_for(fam, rule)
        full = power.pool(model, arm, N, nz, reps, w, rng)
        half = power.pool(sim.Scheduler("mix", base_sched=model, p_rule=0.5), arm, N, nz, reps, w, rng)
        for sname, shares in SPLITS.items():
            counts = split_counts(w, shares)
            kept = [c for c in counts if c >= MIN_N]
            row = {"counts": counts, "windows_in_test": sum(kept), "units_in_test": len(kept)}
            for mode, pools in (("exact", full), ("half", half)):
                ups = [_units(x, counts) for x in pools]
                row[f"power_{mode}_window_weighted"] = float(np.mean([reject(u, names, rule, False) for u in ups]))
                st = strat_cell_stats(ups, names, rule, ceil, rng)
                row[f"power_{mode}_stratified"] = st["power"]
                if mode == "exact":
                    row["thresholds_stratified"] = {"strict_mean": _q(st["_m"], 0.05),
                                                    "strict_ci_lower": _q(st["_l"], 0.05),
                                                    "strict_window_share": _q(st["_s"], 0.05)}
            res["cells"][f"{fam}/{arm}/{rule}/{sname}"] = row
        # dominance: the first unit (60% of the windows) follows the rule, the other is random
        a_n, b_n = split_counts(w, SPLITS["2 units 60/40"])
        dom_a = power.pool(model, arm, N, nz, reps, a_n, rng)
        dom_b = power.pool(sim.RANDOM, arm, N, nz, reps, b_n, rng)
        ups = [[a, b] for a, b in zip(dom_a, dom_b)]
        res["dominance"][f"{fam}/{arm}/{rule}"] = {
            "counts": [a_n, b_n],
            "reject_window_weighted": float(np.mean([reject(u, names, rule, False) for u in ups])),
            "reject_stratified": float(np.mean([reject(u, names, rule, True) for u in ups]))}
        log(f"[{level}] cell {fam}/{arm}/{rule} done {time.time() - t0:.0f}s")
    # false-positive rate under the null (all units random), any rule of the arm, Holm
    for arm in w_pool:
        names = rules.RULES_BY_ARM[arm]
        nul = power.pool(sim.RANDOM, arm, N, nz, reps, w_pool[arm], rng)
        for sname, shares in SPLITS.items():
            ups = [_units(x, split_counts(w_pool[arm], shares)) for x in nul]
            res["fpr"][f"{arm}/{sname}"] = {
                mode: float(np.mean([any(v < ALPHA for v in holm_all(u, names, mode == "stratified").values()
                                         if not math.isnan(v)) for u in ups]))
                for mode in ("window_weighted", "stratified")}
    log(f"[{level}] fpr done {time.time() - t0:.0f}s")
    return res


def agave_w_pool(alloc_block: dict) -> dict[str, int]:
    """Usable windows of the pool per arm = the Agave group's expected usable total."""
    return {r["arm"]: int(r["expected_usable_total"]) for r in alloc_block["rows"] if r["group"] == "Agave"}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--sim", default="sim/results/sim_v2.json")
    ap.add_argument("--alloc", default="sim/results/alloc_v2.json")
    ap.add_argument("--nuisance-file", default="sim/results/nuisance_calibrated.json")
    ap.add_argument("--seed", type=int, default=20261022)
    ap.add_argument("--reps", type=int, default=200)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    simd = json.loads(pathlib.Path(a.sim).read_text(encoding="utf-8"))
    alloc = json.loads(pathlib.Path(a.alloc).read_text(encoding="utf-8"))
    scen = calibrate.scenarios_from_file(a.nuisance_file)
    levels = {
        "preset_medium": (sim.MEDIUM, {arm: {r: v["medium"]["mean_tau"] for r, v in rs.items()}
                                       for arm, rs in simd["ceilings"]["agave"].items()},
                          agave_w_pool(alloc["allocation"])),
        "calibrated": (scen["calibrated"], {arm: {r: v["calibrated"]["mean_tau"] for r, v in rs.items()}
                                            for arm, rs in simd["scenarios"]["calibrated"]["ceilings"]["agave"].items()},
                       agave_w_pool(alloc["scenarios"]["calibrated"]["allocation"])),
    }
    out = {"label": "SIMULATION", "seed": a.seed, "reps": a.reps, "n": N, "min_unit_windows": MIN_N,
           "splits": SPLITS, "levels": {}}
    for i, (name, (nz, ceil, wp)) in enumerate(levels.items()):
        out["levels"][name] = run_level(name, nz, ceil, wp, a.reps, a.seed + 100 + i,
                                        lambda m: print(m, flush=True))
    pathlib.Path(a.out).write_text(json.dumps(out, indent=1, sort_keys=True), encoding="utf-8")
    print(f"POOL SIMULATION written to {a.out}")


if __name__ == "__main__":
    main()
