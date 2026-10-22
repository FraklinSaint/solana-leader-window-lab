"""Estimate the simulator's nuisance parameters from CALIBRATION data (exploratory).

``python -m slwlab.calibrate --data ../data --out sim/results/nuisance_calibrated.json``

Pre-registered procedure (PREREGISTRATION §0). It uses only **order-agnostic** statistics.
The order of our txs in calibration blocks is never used, so calibration cannot tune a
ceiling toward an observed ordering:

| parameter | estimator |
|---|---|
| p_land | landed / sent, ordering-eligible calibration txs (any slot) |
| clock_sd_ms | 1.4826 * MAD of `err_prep_ms` over all calibration and dry-run clock.jsonl |
| offset_ms | the calibration offset with the highest usable-window rate |
| bg_between | the value whose simulated median block span of our landed txs (Agave model, Arm P) is closest to the observed median span (blocks of usable Agave-family Arm P/R calibration windows) |
| jitter_ms, exec_sigma, busy_max_ms | not identifiable without using order: kept at the preset levels |

Every estimate is compared with the preset low/high range. A value outside it is flagged
`deviation: true`, and REPORT.md must carry it as a labeled deviation.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import pathlib
from collections import defaultdict

import numpy as np

from . import parse, sim
from .analyze import FAMILY_OF_STRATUM

BG_GRID = [0.25, 0.5, 1, 2, 3, 5, 8, 10, 15, 20, 30]


def sim_median_span(bg: float, rng: np.random.Generator, k: int = 400) -> float:
    nz = sim.with_nuisance(sim.MEDIUM, bg_between=bg)
    spans = [w.span for w in (sim.simulate_window("P", sim.AGAVE, nz, rng) for _ in range(k)) if w.span]
    return float(np.median(spans))


def preset_range(field: str) -> tuple[float, float]:
    vals = [getattr(v, field) for v in sim.NUISANCE_GRID.values()]
    return min(vals), max(vals)


def estimate(data_root: pathlib.Path, seed: int = 20261022) -> dict:
    rng = np.random.default_rng(seed)
    lines = parse.read_jsonl(data_root / "exploratory" / "calibration_manifest.jsonl")
    eligible = [l for l in lines if l["tx"].get("ordering_eligible", True)]
    out: dict = {"label": "EXPLORATORY (calibration-derived nuisance; SIMULATION input)",
                 "n_calibration_txs": len(eligible), "estimates": {}}

    def put(field, value, method, n):
        lo, hi = preset_range(field) if value is not None else (None, None)
        out["estimates"][field] = {
            "value": value, "method": method, "n": n, "preset_low": lo, "preset_high": hi,
            "deviation": bool(value is not None and not (lo <= value <= hi))}

    # landing
    landed = sum(1 for l in eligible if l["outcome"]["status"] == "landed")
    put("p_land", landed / len(eligible) if eligible else None, "landed / sent", len(eligible))
    by_fam = defaultdict(lambda: [0, 0])
    for l in eligible:
        f = FAMILY_OF_STRATUM.get(l["tx"]["stratum"], "other")
        by_fam[f][0] += l["outcome"]["status"] == "landed"
        by_fam[f][1] += 1
    out["p_land_by_family"] = {f: a / b for f, (a, b) in by_fam.items()}

    # clock error
    errs = [c["err_prep_ms"] for p in sorted((data_root / "logs").glob("*/clock.jsonl"))
            for c in parse.read_jsonl(p) if c.get("err_prep_ms") is not None]
    if errs:
        e = np.array(errs)
        put("clock_sd_ms", float(1.4826 * np.median(np.abs(e - np.median(e)))), "1.4826 MAD of err_prep_ms", len(e))
    else:
        put("clock_sd_ms", None, "no clock.jsonl", 0)

    # offset with the best usable rate
    windows = parse.windows_from_manifest(eligible)
    by_off = defaultdict(lambda: [0, 0])
    for w in windows:
        by_off[w.offset_ms][0] += w.usable
        by_off[w.offset_ms][1] += 1
    if by_off:
        rates = {o: a / b for o, (a, b) in by_off.items()}
        best = max(rates, key=lambda o: (rates[o], -abs(o)))
        out["usable_rate_by_offset"] = rates
        out["estimates"]["offset_ms"] = {"value": float(best), "method": "argmax usable rate", "n": len(windows),
                                         "deviation": False}

    # background load from block spans (order-agnostic)
    spans = []
    for w in windows:
        if w.usable and w.arm in ("P", "R") and FAMILY_OF_STRATUM.get(w.stratum) == "agave":
            spans.append(max(w.positions) - min(w.positions) + 1)
    if spans:
        obs = float(np.median(spans))
        table = {bg: sim_median_span(bg, rng) for bg in BG_GRID}
        best = min(table, key=lambda bg: abs(table[bg] - obs))
        out["span"] = {"observed_median": obs, "n_windows": len(spans), "simulated_median_by_bg": table}
        put("bg_between", float(best), "match median block span (Agave model, Arm P)", len(spans))
    else:
        put("bg_between", None, "no usable Agave-family P/R calibration windows", 0)

    for f in ("jitter_ms", "exec_sigma", "busy_max_ms"):
        out["estimates"][f] = {"value": None, "method": "not identifiable order-agnostically; preset levels kept",
                               "deviation": False}
    out["any_deviation"] = any(v.get("deviation") for v in out["estimates"].values())
    return out


def calibrated_nuisance(est: dict) -> sim.Nuisance:
    """MEDIUM with every available estimate substituted (the post-calibration primary level)."""
    kw = {}
    for f in ("p_land", "clock_sd_ms", "offset_ms", "bg_between"):
        v = est["estimates"].get(f, {}).get("value")
        if v is not None:
            kw[f] = v
    return dataclasses.replace(sim.MEDIUM, **kw)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    est = estimate(pathlib.Path(a.data))
    pathlib.Path(a.out).write_text(json.dumps(est, indent=1, default=str), encoding="utf-8")
    print(json.dumps({k: v.get("value") for k, v in est["estimates"].items()}))


if __name__ == "__main__":
    main()
