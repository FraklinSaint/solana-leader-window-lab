"""Confirmatory analysis per validator x arm (PREREGISTRATION §6-§9).

``python -m slwlab.analyze --manifest ../data/manifest.jsonl --sim sim/results/sim_v1.json
--out ../data/analysis/results.json``

Inputs are files only: the manifest (measurements) and the frozen SIMULATION results
(ceilings, thresholds, power). The output keeps measured numbers and simulated reference
values in separate, labeled fields.
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib

import numpy as np

from . import parse, rules, stats

FAMILY_OF_STRATUM = {
    "Agave": "agave", "JitoAgave": "agave", "HarmonicAgave": "agave", "BamAgave": "agave",
    "Rakurai": "agave", "Firedancer": "fd2", "HarmonicFiredancer": "fd2", "FireBam": "fd2",
}


MIN_CEILING = 0.3  # power.MIN_CEILING
# STRICT is reported as STRICT_RULE_LOW_SEPARATION where the simulator's false-STRICT rate on
# a half-strength rule exceeds this at the achieved W (e.g. Agave lock-free arms at W = 20).
MAX_FALSE_STRICT = 0.10
GRID_W = (10, 20, 30, 50)


def ceiling(sim: dict, family: str, arm: str, rule: str) -> float | None:
    """tau ceiling of `rule` under "rule + this family's interleaving"; None if the cell is
    not identifiable (then tau is reported raw and the rule cannot be STRICT)."""
    c = sim["ceilings"].get(family, {}).get(arm, {}).get(rule, {}).get("medium")
    if c and c["mean_tau"] >= MIN_CEILING:
        return c["mean_tau"]
    return None


def grid_w(n_windows: int) -> int | None:
    """Largest simulated W not above the achieved number of usable windows."""
    g = [w for w in GRID_W if w <= n_windows]
    return g[-1] if g else None


def cell_ref(sim: dict, family: str, arm: str, rule: str, n: int, n_windows: int) -> dict | None:
    w = grid_w(n_windows)
    if w is None:
        return None
    return sim["cells"].get(f"{family}/{arm}/{rule}/N{n}/W{w}")


def window_scores(w: parse.Window, names: list[str]) -> dict[str, tuple[list[float], float]]:
    out = {}
    arrival = [x / 1000.0 for x in w.send_ns]  # rules take microseconds
    for r in names:
        sc = rules.score(r, w.landed, arrival if r in ("R_FIFO", "R_FBA50", "TIE_ARRIVAL_FIFO",
                                                      "TIE_ARRIVAL_LIFO") else None)
        out[r] = (sc, stats.tau_vs_positions(sc, w.positions))
    return out


def power_ok(sim: dict, family: str, arm: str, rule: str, n: int, n_windows: int) -> bool:
    """>= 90% power to detect a half-strength version of `rule` at this window count."""
    ref = cell_ref(sim, family, arm, rule, n, n_windows)
    return bool(ref and ref["power_half"] >= 0.9)


def analyze(lines: list[dict], sim: dict, seed: int = 20261022) -> dict:
    rng = np.random.default_rng(seed)
    windows = parse.windows_from_manifest(lines)
    out = {"label": "MEASUREMENT", "simulation_ref": {"seed": sim["seed"], "label": "SIMULATION"},
           "cells": []}
    for (identity, arm), ws in sorted(parse.by_validator_arm(windows).items()):
        usable = [w for w in ws if w.usable]
        sent = sum(w.n for w in ws)
        landed = sum(1 for w in ws for l in w.lines if l["outcome"]["status"] == "landed")
        cell = {
            "identity": identity, "arm": arm, "stratum": ws[0].stratum,
            "windows_sent": len(ws), "windows_usable": len(usable),
            "landing_rate": landed / sent if sent else None,
            "exclusions": dict(parse.exclusion_counts([l for w in ws for l in w.lines])),
            "window_reasons": dict(parse.Counter(w.reason for w in ws if not w.usable)),
            "dates": sorted({w.date for w in usable}),
            "rules": {},
        }
        if usable:
            fam = FAMILY_OF_STRATUM.get(ws[0].stratum, "agave")
            names = rules.RULES_BY_ARM[arm]
            per = [window_scores(w, names) for w in usable]
            pvals = {r: stats.randomization_pvalue([p[r][0] for p in per], [p[r][1] for p in per], rng)
                     for r in names}
            ph = stats.holm(pvals)
            n = usable[0].n
            for r in names:
                taus = np.array([p[r][1] for p in per], float)
                c = ceiling(sim, fam, arm, r)
                tn = taus / c if c else taus
                ci = stats.bootstrap_ci(tn, rng)
                ref = cell_ref(sim, fam, arm, r, n, len(usable)) if c else None
                th = ref["thresholds"] if ref else None
                cell["rules"][r] = {
                    "mean_tau": float(np.nanmean(taus)), "tau_ceiling_sim": c, "identifiable": c is not None,
                    "mean_tau_norm": float(np.nanmean(tn)) if c else None, "ci95": ci,
                    "p_one_sided": pvals[r], "p_holm": ph[r],
                    "share_windows_tau_norm_ge_0.5": float(np.nanmean(tn >= 0.5)) if c else None,
                    "share_perfect": float(np.nanmean(taus >= 1 - 1e-12)),
                    "thresholds_sim": th,
                    "verdict": stats.verdict(tn, ph[r] if not math.isnan(ph[r]) else 1.0, ci, th,
                                             power_ok(sim, fam, arm, r, n, len(usable))),
                    "p_false_strict_sim": ref.get("p_strict_half") if ref else None,
                }
                rr = cell["rules"][r]
                if rr["verdict"] == "STRICT_RULE" and (rr["p_false_strict_sim"] or 0) > MAX_FALSE_STRICT:
                    rr["verdict"] = "STRICT_RULE_LOW_SEPARATION"
            ranked = sorted(names, key=lambda r: -cell["rules"][r]["mean_tau"])
            if len(ranked) >= 2:
                a, b = ranked[0], ranked[1]
                d, ci = stats.paired_bootstrap([p[a][1] for p in per], [p[b][1] for p in per], rng)
                cell["top_two"] = {"first": a, "second": b, "diff_mean_tau": d, "ci95": ci}
        out["cells"].append(cell)
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--sim", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    lines = parse.read_jsonl(pathlib.Path(a.manifest))
    sim = json.loads(pathlib.Path(a.sim).read_text(encoding="utf-8"))
    res = analyze(lines, sim)
    pathlib.Path(a.out).parent.mkdir(parents=True, exist_ok=True)
    pathlib.Path(a.out).write_text(json.dumps(res, indent=1), encoding="utf-8")
    print(f"{len(res['cells'])} validator x arm cells -> {a.out}")


if __name__ == "__main__":
    main()
