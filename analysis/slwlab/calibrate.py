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
| jitter_ms | not identifiable from order; kept at the preset level. The session's RTT record (``data/sessions/*/rtt.jsonl``, Wi-Fi limitation) gives an order-agnostic **proxy**: robust RTT sigma / sqrt(2) as a one-way sd. It is an upper bound for per-tx jitter inside a burst (a burst shares most of the path delay), reported with `proxy_above_preset_high`; if it exceeds the preset high level, the simulator is also run at that level as a labeled Wi-Fi sensitivity (PREREGISTRATION "Network path and platform strata") |
| exec_sigma, busy_max_ms | not identifiable without using order: kept at the preset levels |

Every estimate is compared with the preset low/high range. A value outside it is flagged
`deviation: true`, and REPORT.md must carry it as a labeled deviation.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import math
import pathlib
from collections import defaultdict

import numpy as np

from . import parse, sim
from .analyze import FAMILY_OF_STRATUM

MIN_OFFSET_WINDOWS = 30
BG_GRID = [0.25, 0.5, 1, 2, 3, 5, 8, 10, 15, 20, 30]


def sim_median_span(bg: float, rng: np.random.Generator, k: int = 400) -> float:
    nz = sim.with_nuisance(sim.MEDIUM, bg_between=bg)
    spans = [w.span for w in (sim.simulate_window("P", sim.AGAVE, nz, rng) for _ in range(k)) if w.span]
    return float(np.median(spans))


MAX_PAIR_GAP_S = 15


def percentile_nr(sorted_vals: list[float], q: float) -> float | None:
    """Nearest-rank percentile (the definition of crates/sender/src/netmon.rs)."""
    if not sorted_vals:
        return None
    rank = max(1, math.ceil(q * len(sorted_vals)))
    return sorted_vals[min(rank, len(sorted_vals)) - 1]


def rtt_stats(samples: list[tuple[int, float | None]]) -> dict:
    """Same statistics as netmon.rs `rtt_stats`: nearest-rank percentiles, 1.4826 MAD sigma,
    jitter = mean |difference| of consecutive successful probes at most 15 s apart."""
    ok = sorted(r for _, r in samples if r is not None)
    diffs = [abs(b - a) for (t0, a), (t1, b) in zip(samples, samples[1:])
             if a is not None and b is not None and t1 - t0 <= MAX_PAIR_GAP_S]
    med = percentile_nr(ok, 0.5)
    mad = percentile_nr(sorted(abs(x - med) for x in ok), 0.5) if med is not None else None
    return {"n": len(samples), "n_ok": len(ok), "n_err": len(samples) - len(ok),
            "min_ms": ok[0] if ok else None, "p50_ms": med,
            "p90_ms": percentile_nr(ok, 0.90), "p95_ms": percentile_nr(ok, 0.95),
            "p99_ms": percentile_nr(ok, 0.99), "max_ms": ok[-1] if ok else None,
            "sigma_mad_ms": 1.4826 * mad if mad is not None else None,
            "jitter_ms": sum(diffs) / len(diffs) if diffs else None, "n_pairs": len(diffs)}


def read_rtt(data_root: pathlib.Path, modes: tuple[str, ...] = ("calibrate",)) -> dict[str, list]:
    """Probes per session (warmup probes excluded) of sessions in the given modes."""
    out: dict[str, list] = {}
    for sdir in sorted((data_root / "sessions").glob("*")):
        rtt = sdir / "rtt.jsonl"
        meta = sdir / "session.json"
        if not rtt.exists() or not meta.exists():
            continue
        if json.loads(meta.read_text(encoding="utf-8")).get("mode") not in modes:
            continue
        out[sdir.name] = [(r["unix"], r.get("rtt_ms")) for r in parse.read_jsonl(rtt)
                          if not r.get("warmup") and "unix" in r]
    return out


def network_path(data_root: pathlib.Path) -> dict:
    """RTT distribution of the calibration sessions (all pooled and per session) and the
    one-way jitter proxy. Reported, and used only as a labeled simulator sensitivity."""
    per = read_rtt(data_root)
    pooled = [s for v in per.values() for s in v]
    stats = rtt_stats(pooled)
    sigma = stats["sigma_mad_ms"]
    proxy = sigma / math.sqrt(2) if sigma is not None else None
    _, hi = preset_range("jitter_ms")
    return {"label": "EXPLORATORY (Wi-Fi path record; proxy, not the delay of QUIC sends to a leader)",
            "sessions": sorted(per), "rtt": stats,
            "rtt_by_session": {k: rtt_stats(v) for k, v in per.items()},
            "oneway_jitter_proxy_ms": proxy, "preset_jitter_high_ms": hi,
            "proxy_above_preset_high": bool(proxy is not None and proxy > hi)}


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
        out["windows_by_offset"] = {o: b for o, (a, b) in by_off.items()}
        # The offset the simulation uses: the best usable rate among offsets with enough windows.
        # An offset seen in only a few windows is not part of the sweep: Arm F's extra lead moves
        # its effective offset (arms.rs), so a few F-only offsets can top the argmax above.
        big = {o: r for o, r in rates.items() if by_off[o][1] >= MIN_OFFSET_WINDOWS}
        if big:
            used = max(big, key=lambda o: (big[o], -abs(o)))
            small = {o: by_off[o][1] for o in sorted(rates) if o not in big}
            out["offset_used"] = {
                "value": float(used), "n_windows": by_off[used][1], "usable_rate": big[used],
                "min_windows": MIN_OFFSET_WINDOWS,
                "excluded_offsets_windows": small,
                "reason": (f"best usable rate among offsets with >= {MIN_OFFSET_WINDOWS} windows. "
                           f"Excluded for fewer windows: {small}. Arm F is sent earlier by its extra "
                           f"lead, which shifts its effective offset off the sweep, so those offsets are "
                           f"Arm F only; the argmax over all offsets ({float(best)}) is not used.")}
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

    out["network_path"] = network_path(data_root)
    for f in ("jitter_ms", "exec_sigma", "busy_max_ms"):
        out["estimates"][f] = {"value": None, "method": "not identifiable order-agnostically; preset levels kept",
                               "deviation": False}
    out["estimates"]["jitter_ms"]["method"] += (
        "; RTT proxy in network_path (upper bound, never a deviation by itself)")
    out["any_deviation"] = any(v.get("deviation") for v in out["estimates"].values())
    return out


def calibrated_nuisance(est: dict) -> sim.Nuisance:
    """MEDIUM with every available estimate substituted (the post-calibration primary level)."""
    kw = {}
    for f in ("p_land", "clock_sd_ms", "offset_ms", "bg_between"):
        v = est["estimates"].get(f, {}).get("value")
        if v is not None:
            kw[f] = v
    if "offset_used" in est:  # never the all-offsets argmax, see estimate()
        kw["offset_ms"] = est["offset_used"]["value"]
    return dataclasses.replace(sim.MEDIUM, **kw)


def wifi_sensitivity_nuisance(est: dict) -> sim.Nuisance | None:
    """The calibrated level with per-tx jitter raised to the RTT-derived one-way proxy, or
    None when the proxy does not exceed the preset high jitter (no extra run needed)."""
    path = est.get("network_path", {})
    if not path.get("proxy_above_preset_high"):
        return None
    return dataclasses.replace(calibrated_nuisance(est), jitter_ms=float(path["oneway_jitter_proxy_ms"]))


def scenarios_from_file(path: str) -> dict[str, sim.Nuisance]:
    """Extra simulation scenarios from a nuisance_calibrated.json (read only): the calibrated point
    and, when the RTT jitter proxy exceeds the preset high level, the Wi-Fi sensitivity point."""
    est = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
    out = {"calibrated": calibrated_nuisance(est)}
    wifi = wifi_sensitivity_nuisance(est)
    if wifi is not None:
        out["wifi_sensitivity"] = wifi
    return out


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
