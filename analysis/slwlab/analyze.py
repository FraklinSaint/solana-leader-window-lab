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

from . import blocklist, independence, parse, rules, sim as simmod, stats

FAMILY_OF_STRATUM = {
    "Agave": "agave", "JitoAgave": "agave", "HarmonicAgave": "agave", "BamAgave": "agave",
    "Rakurai": "agave", "Firedancer": "fd2", "HarmonicFiredancer": "fd2", "BamFiredancer": "fd2",
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


def analyze(lines: list[dict], sim: dict, seed: int = 20261022, present: dict[str, set[int]] | None = None,
            include_skipped: bool = False, require_slot_status: bool = False) -> dict:
    """Per validator x arm. `present` (window_id -> slots with a block, from the stored getBlocks
    lists) turns on the §9 skipped-slot exclusion; `include_skipped=True` is its sensitivity run."""
    rng = np.random.default_rng(seed)
    windows = parse.windows_from_manifest(lines, present, include_skipped, require_slot_status)
    out = {"label": "MEASUREMENT", "simulation_ref": {"seed": sim["seed"], "label": "SIMULATION"},
           "skipped_slots_included": include_skipped,
           "skipped_slots_by_validator": parse.skipped_report(windows),
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


def analyze_pooled(lines: list[dict], sim: dict, stratum: str = "Agave", seed: int = 20261022,
                   present: dict[str, set[int]] | None = None, include_skipped: bool = False,
                   require_slot_status: bool = False, ref_reps: int = 200,
                   nuisance: simmod.Nuisance = simmod.MEDIUM) -> dict:
    """PREREGISTRATION §8 option B: one pooled, STRATIFIED verdict per arm x rule for `stratum`.

    Units = independence units (§6c: validators that share a TPU IP are one unit). The
    statistic is the mean over units (with >= MIN_UNIT_WINDOWS usable windows in the cell) of the
    unit's mean window tau, so no unit carries the verdict because it has more windows. Per
    validator numbers are DESCRIPTIVE ONLY (no p-value, no verdict). Thresholds and the
    half-strength power come from a SIMULATION at the actual window counts per unit."""
    from . import poolpower  # lazy: poolpower -> power -> calibrate -> analyze

    rng = np.random.default_rng(seed)
    windows = parse.windows_from_manifest(lines, present, include_skipped, require_slot_status)
    unit_of = independence.units_from_manifest(lines)
    pool = [w for w in windows if w.stratum == stratum]
    fam = FAMILY_OF_STRATUM.get(stratum, "agave")
    out = {"label": "MEASUREMENT", "pooled_stratum": stratum,
           "design": "option B: stratified by independence unit, equal unit weights",
           "min_unit_windows": stats.MIN_UNIT_WINDOWS, "skipped_slots_included": include_skipped,
           "units": {u: sorted(i for i, x in unit_of.items() if x == u and any(w.identity == i for w in pool))
                     for u in sorted({unit_of[w.identity] for w in pool})},
           "skipped_slots_by_validator": parse.skipped_report(pool), "cells": []}
    for arm in sorted({w.arm for w in pool}):
        ws = [w for w in pool if w.arm == arm]
        usable = [w for w in ws if w.usable]
        names = rules.RULES_BY_ARM[arm]
        units = sorted({unit_of[w.identity] for w in usable})
        per_unit = {u: [window_scores(w, names) for w in usable if unit_of[w.identity] == u] for u in units}
        kept = stats.stratified_units([[p[names[0]][1] for p in per_unit[u]] for u in units])
        cell = {"arm": arm, "windows_sent": len(ws), "windows_usable": len(usable),
                "usable_by_unit": {u: len(per_unit[u]) for u in units},
                "units_in_test": [units[k] for k in kept], "dates": sorted({w.date for w in usable}),
                "by_validator_descriptive": {}, "rules": {}}
        for ident in sorted({w.identity for w in ws}):
            wi = [w for w in ws if w.identity == ident]
            ui = [w for w in wi if w.usable]
            sc = [window_scores(w, names) for w in ui]
            sent = sum(w.n for w in wi)
            landed = sum(1 for w in wi for l in w.lines if l["outcome"]["status"] == "landed")
            cell["by_validator_descriptive"][ident] = {
                "label": "DESCRIPTIVE ONLY", "windows_sent": len(wi), "windows_usable": len(ui),
                "landing_rate": landed / sent if sent else None,
                "mean_tau": {r: float(np.nanmean([p[r][1] for p in sc])) if sc else None for r in names}}
        if len(kept) >= 2:
            pv = {r: stats.stratified_randomization_pvalue(
                [[p[r][0] for p in per_unit[u]] for u in units], [[p[r][1] for p in per_unit[u]] for u in units], rng)
                for r in names}
            ph = stats.holm(pv)
            counts = [len(per_unit[units[k]]) for k in kept]
            for r in names:
                taus = [[p[r][1] for p in per_unit[u]] for u in units]
                c = ceiling(sim, fam, arm, r)
                tn = [np.asarray(t, float) / c if c else np.asarray(t, float) for t in taus]
                ci = stats.stratified_bootstrap_ci(tn, rng)
                mean_tn = stats.stratified_mean(tn)
                share = float(np.mean([np.nanmean(tn[k] >= 0.5) for k in kept])) if c else None
                ref = poolpower.cell_reference(fam, arm, r, counts, nuisance, c, ref_reps, rng) if c else None
                th = ref["thresholds"] if ref else None
                v = stats.verdict_values(mean_tn, share if share is not None else float("nan"),
                                         ph[r] if not math.isnan(ph[r]) else 1.0, ci, th,
                                         bool(ref and ref["power_half"] >= 0.9))
                if v == "STRICT_RULE" and ref and (ref["p_strict_half"] or 0) > MAX_FALSE_STRICT:
                    v = "STRICT_RULE_LOW_SEPARATION"
                cell["rules"][r] = {
                    "mean_tau_stratified": stats.stratified_mean(taus), "tau_ceiling_sim": c,
                    "identifiable": c is not None, "mean_tau_norm_stratified": mean_tn if c else None, "ci95": ci,
                    "p_one_sided": pv[r], "p_holm": ph[r], "share_windows_tau_norm_ge_0.5": share,
                    "reference_sim": ref, "verdict": v}
        else:
            cell["note"] = f"fewer than 2 units with >= {stats.MIN_UNIT_WINDOWS} usable windows: no pooled test"
            cell["rules"] = {r: {"verdict": "INCONCLUSIVE"} for r in names}
        out["cells"].append(cell)
    return out


def main(argv: list[str] | None = None) -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--sim", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--data", help="data dir with logs/<run_id>/blocklist.json (§9 skipped slots); "
                                   "required when the manifest holds confirmatory (measure) windows")
    ap.add_argument("--require-blocklists", action="store_true",
                    help="demand a list for every run, also exploratory ones (confirmatory runs always need one)")
    ap.add_argument("--include-skipped", action="store_true",
                    help="sensitivity analysis: keep windows with a skipped slot")
    ap.add_argument("--pooled", metavar="STRATUM", help="also write the pooled, stratified analysis of this stratum")
    a = ap.parse_args(argv)
    lines = parse.read_jsonl(pathlib.Path(a.manifest))
    sim = json.loads(pathlib.Path(a.sim).read_text(encoding="utf-8"))
    confirmatory = blocklist.runs_with_purpose(lines, "measure")
    needed = sorted({l["tx"]["run_id"] for l in lines}) if a.require_blocklists else confirmatory
    if needed and not a.data:
        raise SystemExit("this manifest needs the stored getBlocks lists (PREREGISTRATION §9): pass --data <data dir>")
    present = None
    if a.data:
        data = pathlib.Path(a.data)
        missing = [r for r in needed if blocklist.load(data, r) is None]
        if missing:
            raise SystemExit(
                "no stored getBlocks list for run(s) " + ", ".join(missing) + f" (expected {blocklist.FILE} in "
                "data/logs/<run_id>/). Windows with a skipped slot cannot be told apart without it, so no "
                "analysis is written. Run: python -m slwlab.blocklist fetch --data <data> --manifest <manifest> "
                "(RUNBOOK_MAINNET S8 step 2).")
        present = blocklist.present_map(data, sorted({l["tx"]["run_id"] for l in lines}))
    res = analyze(lines, sim, present=present, include_skipped=a.include_skipped,
                  require_slot_status=bool(needed))
    pathlib.Path(a.out).parent.mkdir(parents=True, exist_ok=True)
    pathlib.Path(a.out).write_text(json.dumps(res, indent=1), encoding="utf-8")
    print(f"{len(res['cells'])} validator x arm cells -> {a.out}")
    if a.pooled:
        pooled = analyze_pooled(lines, sim, a.pooled, present=present, include_skipped=a.include_skipped,
                                require_slot_status=a.require_blocklists)
        p = pathlib.Path(a.out).with_name(pathlib.Path(a.out).stem + f"_pooled_{a.pooled}.json")
        p.write_text(json.dumps(pooled, indent=1), encoding="utf-8")
        print(f"{len(pooled['cells'])} pooled {a.pooled} arm cells -> {p}")


if __name__ == "__main__":
    main()
