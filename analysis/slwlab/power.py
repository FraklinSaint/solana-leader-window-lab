"""SIMULATION: tau ceilings, power, false-positive rate and verdict thresholds.

Everything here fills the TBD(sim) slots of docs/PREREGISTRATION.md. The procedure is
fixed in code and the seed is recorded, so the numbers are reproducible:
``python -m slwlab.power --out sim/results/sim_v1.json`` (then ``python -m slwlab.simreport``).

A *cell* is (client family, arm, rule). Its model is "rule R decides the pop order, plus
this family's thread/microblock interleaving" (sim.Scheduler.pop_rule). Harmonic's builder
strategies are separate families (fifo, fba50) with no interleaving. For every cell:
- ceiling = mean tau of R under its own model (medium nuisance). tau_norm = tau / ceiling.
  A cell with ceiling < MIN_CEILING is *not identifiable*: R cannot be told apart there, and
  it can never be STRICT.
- power = P(R significant after Holm across the arm's rules) for the exact model and for a
  half-strength model (each window follows the model with prob. 0.5, otherwise random).
- STRICT thresholds = 5th percentiles (over replications) of mean tau_norm, of its bootstrap
  CI lower bound, and of the share of windows with tau_norm >= 0.5, under the exact model.
  Then P(STRICT) is reported for the exact and the half-strength model.
"""

from __future__ import annotations

import argparse
import json
import math
import time
from collections import Counter
from dataclasses import asdict, replace

import numpy as np

from . import calibrate, rules, sim, stats
from .model import ArmConfig, design

ARMS = ["P", "X", "R", "C", "F"]
NS = [6, 8, 10]
WS = [10, 20, 30, 50]
ALPHA = 0.05
WINDOW_TAU_NORM = 0.5
MIN_CEILING = 0.3
LAMPORTS_PER_SIGNATURE = 5000
THREAD_FAMILIES = ["agave", "fd2"]
SENSITIVITY_FAMILIES = ["fd4"]
BUILDER_FAMILIES = {"fifo": "R_FIFO", "fba50": "R_FBA50"}  # Harmonic strategies
ARRIVAL_RULES = ("R_FIFO", "R_FBA50", "TIE_ARRIVAL_FIFO", "TIE_ARRIVAL_LIFO")


def usable_min(n: int) -> int:
    """>= 6 of 8 (PREREGISTRATION §9), scaled with N."""
    return math.ceil(0.75 * n)


def usable_rate(n: int, p: float) -> float:
    k0 = usable_min(n)
    return float(sum(math.comb(n, k) * p**k * (1 - p) ** (n - k) for k in range(k0, n + 1)))


def null_var_tau(score) -> float:
    """Exact null variance of tau-b between `score` (ties allowed) and an untied random
    order: Var(S) = [n(n-1)(2n+5) - sum t(t-1)(2t+5)] / 18, tau_b = S / sqrt(n0 (n0 - n1))."""
    n = len(score)
    ties = [c for c in Counter(score).values() if c > 1]
    var_s = (n * (n - 1) * (2 * n + 5) - sum(t * (t - 1) * (2 * t + 5) for t in ties)) / 18
    n0 = n * (n - 1) / 2
    n1 = sum(t * (t - 1) / 2 for t in ties)
    den = n0 * (n0 - n1)
    return var_s / den if den > 0 else float("nan")


def model_for(family: str, rule: str) -> sim.Scheduler:
    if family in BUILDER_FAMILIES:
        return sim.SCHEDULERS[family]
    return replace(sim.SCHEDULERS[family], pop_rule=rule)


def cells() -> list[tuple[str, str, str]]:
    out = []
    for fam in THREAD_FAMILIES + SENSITIVITY_FAMILIES:
        for arm in ["P", "X", "R", "C"]:
            for r in rules.RULES_BY_ARM[arm]:
                out.append((fam, arm, r))
    for fam, r in BUILDER_FAMILIES.items():
        for arm in ["P", "X", "R", "C"]:
            if r in rules.RULES_BY_ARM[arm]:
                out.append((fam, arm, r))
    return out


def window_taus(w: sim.SimWindow, names: list[str], rng: np.random.Generator) -> dict:
    """rule -> (tau, null variance, score) for one simulated usable window."""
    txs = [w.design.txs[i] for i in w.landed]
    arrival = [w.arrival_us[i] for i in w.landed]
    pos = list(range(len(txs)))
    out = {}
    for r in names:
        if r in ("TIE_SIGNATURE", "TIE_PAYER"):
            sc = list(rng.permutation(len(txs)).astype(float))  # keys independent of order
        else:
            sc = rules.score(r, txs, arrival if r in ARRIVAL_RULES else None)
        out[r] = (stats.tau_vs_positions(sc, pos), null_var_tau(sc), sc)
    return out


def pool(sch: sim.Scheduler, arm: str, n: int, nz: sim.Nuisance, reps: int, w_max: int,
         rng: np.random.Generator, x_gap_ms: int | None = None) -> list[list[dict]]:
    cfg = ArmConfig(n=n)
    names = rules.RULES_BY_ARM[arm]
    out = []
    for _ in range(reps):
        wins = []
        while len(wins) < w_max:
            w = sim.simulate_window(arm, sch, nz, rng, cfg, x_gap_ms=x_gap_ms)
            if len(w.landed) >= usable_min(n):
                wins.append(window_taus(w, names, rng))
        out.append(wins)
    return out


def holm_normal(wins: list[dict], names: list[str]) -> dict[str, float]:
    """Holm-adjusted one-sided p-values, normal approximation of the sum of window taus under
    each window's exact null variance (fast; validated against the exact randomization test
    in tests/test_stats.py and by `fpr_exact` below)."""
    p = {}
    for r in names:
        t = [x[r][0] for x in wins if not math.isnan(x[r][0])]
        v = [x[r][1] for x in wins if not math.isnan(x[r][0])]
        p[r] = 0.5 * math.erfc(sum(t) / math.sqrt(sum(v)) / math.sqrt(2)) if t and sum(v) > 0 else float("nan")
    return stats.holm(p)


def holm_exact(wins: list[dict], names: list[str], rng: np.random.Generator) -> dict[str, float]:
    p = {r: stats.randomization_pvalue([x[r][2] for x in wins], [x[r][0] for x in wins], rng, draws=5_000)
         for r in names}
    return stats.holm(p)


def cell_stats(pools: list[list[dict]], names: list[str], rule: str, w: int, ceiling: float | None,
               rng: np.random.Generator, boot: int = 1000) -> dict:
    sig = 0
    means, lows, shares = [], [], []
    for wins in pools:
        sub = wins[:w]
        if holm_normal(sub, names).get(rule, 1.0) < ALPHA:
            sig += 1
        if ceiling:
            tn = np.array([x[rule][0] for x in sub]) / ceiling
            tn = tn[~np.isnan(tn)]
            means.append(float(tn.mean()))
            lows.append(stats.bootstrap_ci(tn, rng, boot)[0])
            shares.append(float(np.mean(tn >= WINDOW_TAU_NORM)))
    return {"power": sig / len(pools), "_m": means, "_l": lows, "_s": shares}


def strict_rate(s: dict, th: dict) -> float | None:
    if not s["_m"]:
        return None
    ok = [m >= th["strict_mean"] and lo >= th["strict_ci_lower"] and sh >= th["strict_window_share"]
          for m, lo, sh in zip(s["_m"], s["_l"], s["_s"])]
    return float(np.mean(ok))


def spend_per_window(n: int) -> dict[str, float]:
    """Average total fee (lamports) of one window per arm (all txs landing)."""
    from .model import priority_fee

    rng = np.random.default_rng(0)
    cfg = ArmConfig(n=n)
    out = {}
    for arm in ARMS:
        d = design(cfg, arm, rng, x_gap_ms=0)
        out[arm] = float(sum(LAMPORTS_PER_SIGNATURE + priority_fee(t.price, t.cu_limit) for t in d.txs))
    out["mean"] = float(np.mean([out[a] for a in ARMS]))
    return out


def _run(seed: int, reps: int, log, primary: sim.Nuisance, grid: dict[str, sim.Nuisance], pkey: str) -> dict:
    """One full simulation. `primary` is the nuisance level of power, thresholds, FPR and Arm F/X
    (the pre-registered MEDIUM), `grid` the levels of ceilings and the positive control, `pkey` the
    grid key of `primary`. The defaults of `run` are the pre-registered ones."""
    t0 = time.time()
    rng = np.random.default_rng(seed)
    res: dict = {
        "label": "SIMULATION", "seed": seed, "reps": reps,
        "alpha_one_sided_holm": ALPHA, "window_tau_norm": WINDOW_TAU_NORM, "min_ceiling": MIN_CEILING,
        "usable_min": {n: usable_min(n) for n in NS},
        "nuisance": {k: asdict(v) for k, v in grid.items()},
        "schedulers": {k: asdict(v) for k, v in sim.SCHEDULERS.items()},
        "spend_per_window_lamports": {n: spend_per_window(n) for n in NS},
        "usable_rate": {n: {lvl: usable_rate(n, nz.p_land) for lvl, nz in grid.items()} for n in NS},
        "ceilings": {}, "cells": {}, "fpr": {}, "fpr_exact": {}, "arm_f": {}, "x_curve": {},
        "positive_control": {},
    }
    # 1) ceilings of every cell (N = 8, 400 windows, each nuisance level)
    for fam, arm, rule in cells():
        for lvl, nz in grid.items():
            pl = pool(model_for(fam, rule), arm, 8, nz, 1, 400, rng)[0]
            t = np.array([x[rule][0] for x in pl])
            res["ceilings"].setdefault(fam, {}).setdefault(arm, {}).setdefault(rule, {})[lvl] = {
                "mean_tau": float(np.nanmean(t)), "sd_tau": float(np.nanstd(t))}
    log(f"ceilings done {time.time() - t0:.0f}s")
    # 2) power and STRICT thresholds for identifiable cells
    for fam, arm, rule in cells():
        ceil = res["ceilings"][fam][arm][rule][pkey]["mean_tau"]
        if ceil < MIN_CEILING or fam in SENSITIVITY_FAMILIES:
            continue
        names = rules.RULES_BY_ARM[arm]
        model = model_for(fam, rule)
        for n in NS:
            full = pool(model, arm, n, primary, reps, max(WS), rng)
            half = pool(sim.Scheduler("mix", base_sched=model, p_rule=0.5), arm, n, primary, reps, max(WS), rng)
            for w in WS:
                sf = cell_stats(full, names, rule, w, ceil, rng)
                sh = cell_stats(half, names, rule, w, ceil, rng)
                th = {"alpha": ALPHA, "window_tau_norm": WINDOW_TAU_NORM,
                      "strict_mean": round(float(np.quantile(sf["_m"], 0.05)), 2),
                      "strict_ci_lower": round(float(np.quantile(sf["_l"], 0.05)), 2),
                      "strict_window_share": round(float(np.quantile(sf["_s"], 0.05)), 2)}
                res["cells"][f"{fam}/{arm}/{rule}/N{n}/W{w}"] = {
                    "ceiling": ceil, "power_exact": sf["power"], "power_half": sh["power"],
                    "thresholds": th, "p_strict_exact": strict_rate(sf, th), "p_strict_half": strict_rate(sh, th)}
        log(f"cell {fam}/{arm}/{rule} done {time.time() - t0:.0f}s")
    # 3) false-positive rate under the null: normal approximation, and the exact test (W <= 20)
    for arm in ARMS:
        names = rules.RULES_BY_ARM[arm]
        for n in NS:
            nul = pool(sim.RANDOM, arm, n, primary, reps, max(WS), rng)
            for w in WS:
                res["fpr"][f"{arm}/N{n}/W{w}"] = float(np.mean(
                    [any(v < ALPHA for v in holm_normal(x[:w], names).values() if not math.isnan(v)) for x in nul]))
                if w <= 20:
                    res["fpr_exact"][f"{arm}/N{n}/W{w}"] = float(np.mean(
                        [any(v < ALPHA for v in holm_exact(x[:w], names, rng).values() if not math.isnan(v))
                         for x in nul]))
    log(f"fpr done {time.time() - t0:.0f}s")
    # 4) Arm F: Firedancer predicts LIFO among exact ties; Agave predicts no determinant
    names = rules.RULES_BY_ARM["F"]
    for fam in ["fd2", "agave"]:
        for n in NS:
            pl = pool(sim.SCHEDULERS[fam], "F", n, primary, reps, max(WS), rng)
            for w in WS:
                ph = [holm_normal(x[:w], names) for x in pl]
                res["arm_f"][f"{fam}/N{n}/W{w}"] = {
                    r: float(np.mean([p[r] < ALPHA for p in ph])) for r in names}
    # 5) Arm X gap response: mean tau of R_PRICE and R_FIFO per send gap, per family (N = 8)
    for fam in ["agave", "fd2", "fifo", "fba50"]:
        for g in ArmConfig().x_gaps_ms:
            pl = pool(sim.SCHEDULERS[fam], "X", 8, primary, 1, 300, rng, x_gap_ms=g)[0]
            res["x_curve"].setdefault(fam, {})[str(g)] = {
                r: float(np.nanmean([x[r][0] for x in pl])) for r in ("R_PRICE", "R_FIFO")}
    # 6) positive control: 95% predictive interval of the mean raw tau of R_RATIO_AGAVE, Arm P
    for n in NS:
        for w in WS:
            pc = {}
            for lvl, nz in grid.items():
                pl = pool(sim.AGAVE, "P", n, nz, reps, w, rng)
                means = [np.nanmean([x["R_RATIO_AGAVE"][0] for x in wins]) for wins in pl]
                pc[lvl] = [float(np.quantile(means, 0.025)), float(np.quantile(means, 0.975))]
            pc["union"] = [min(v[0] for v in pc.values()), max(v[1] for v in pc.values())]
            res["positive_control"][f"N{n}/W{w}"] = pc
    res["elapsed_s"] = round(time.time() - t0, 1)
    return res


def run(seed: int, reps: int, log=print, scenarios: dict[str, sim.Nuisance] | None = None) -> dict:
    """The pre-registered simulation (MEDIUM primary, low/medium/high grid). `scenarios` adds
    extra nuisance points, each a complete separate simulation with that point as its primary
    level and its own derived seed (seed + i), stored under `scenarios`; nothing in the base
    result (same seed, same random stream) depends on them."""
    res = _run(seed, reps, log, sim.MEDIUM, sim.NUISANCE_GRID, "medium")
    if scenarios:
        res["scenarios"] = {}
        for i, (name, nz) in enumerate(scenarios.items(), 1):
            sc = _run(seed + i, reps, lambda m, n=name: log(f"[{n}] {m}"), nz, {name: nz}, name)
            sc["scenario"] = name
            res["scenarios"][name] = sc
    return res


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=20261022)
    ap.add_argument("--reps", type=int, default=200)
    ap.add_argument("--out", required=True)
    ap.add_argument("--nuisance-file", help="nuisance_calibrated.json from slwlab.calibrate: adds the "
                    "calibrated point (and the Wi-Fi sensitivity point if the RTT proxy exceeds the preset "
                    "high jitter) as extra scenarios; the pre-registered grid is unchanged")
    a = ap.parse_args()
    scenarios = calibrate.scenarios_from_file(a.nuisance_file) if a.nuisance_file else None
    res = run(a.seed, a.reps, log=lambda m: print(m, flush=True), scenarios=scenarios)
    with open(a.out, "w", encoding="utf-8") as f:
        json.dump(res, f, indent=1, sort_keys=True)
    print(f"SIMULATION written to {a.out} in {res['elapsed_s']} s")


if __name__ == "__main__":
    main()
