"""Positive control (PREREGISTRATION section 7; fixed in docs/ANALYSIS_CLARIFICATIONS_PRE_RUN.md 1a).

    python scripts/positive_control_check.py --results R/results.json --pooled R/results_pooled_Agave.json \
        --alloc analysis/sim/results/alloc_v2.json

Statistic: the plain mean (np.nanmean over windows) of the raw tau of R_RATIO_AGAVE over the usable Arm C
windows of the pooled Agave stratum. `slwlab.analyze` stores a mean per validator, so the plain mean over all
windows is the window-weighted mean of the per-validator means (exact when no window has an undefined tau,
which for R_RATIO_AGAVE in Arm C means no window with all scores tied). The per-validator numbers of the
per-validator table and of the pooled table's descriptive block must agree, otherwise the script stops.
Primary interval: the union of the top-level union of alloc_v2.json (preset low/medium/high) and
scenarios.calibrated.positive_control_agave_C.interval.union. The preset-only interval and the Wi-Fi scenario
interval are printed next to it. Every value is read from the file. The registered W = 15 interval is used as it
is; the achieved number of usable Arm C windows is reported. Pass iff the observed mean lies inside the primary
interval (bounds included). A failed control is reported prominently. Exit code 0 = pass, 1 = fail.
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib

RULE = "R_RATIO_AGAVE"
ARM = "C"
POOL_STRATUM = "Agave"


def intervals(alloc: dict) -> dict:
    top = alloc["positive_control_agave_C"]
    cal = alloc["scenarios"]["calibrated"]["positive_control_agave_C"]
    wifi = alloc["scenarios"]["wifi_sensitivity"]["positive_control_agave_C"]
    t, c = top["interval"]["union"], cal["interval"]["union"]
    return {
        "primary": {"interval": [min(t[0], c[0]), max(t[1], c[1])], "W": [top["W"], cal["W"]],
                    "source": "union of top-level union (preset low/medium/high) and scenarios.calibrated union"},
        "preset_only": {"interval": list(t), "W": top["W"]},
        "calibrated": {"interval": list(c), "W": cal["W"]},
        "wifi_sensitivity": {"interval": list(wifi["interval"]["union"]), "W": wifi["W"]},
    }


def observed(results: dict, pooled: dict) -> dict:
    if pooled.get("pooled_stratum") != POOL_STRATUM:
        raise SystemExit(f"pooled json is for stratum {pooled.get('pooled_stratum')!r}, expected {POOL_STRATUM!r}")
    cells = [c for c in results["cells"] if c["stratum"] == POOL_STRATUM and c["arm"] == ARM]
    pcell = next((c for c in pooled["cells"] if c["arm"] == ARM), None)
    if pcell is None:
        raise SystemExit("pooled json has no Arm C cell")
    per = {}
    for c in cells:
        r = c["rules"].get(RULE)
        per[c["identity"]] = (int(c["windows_usable"]), r["mean_tau"] if r else None)
    desc = {i: (int(d["windows_usable"]), (d["mean_tau"] or {}).get(RULE))
            for i, d in pcell["by_validator_descriptive"].items()}
    for ident, (n, m) in per.items():
        n2, m2 = desc.get(ident, (None, None))
        same = n == n2 and ((m is None and m2 is None) or (m is not None and m2 is not None and abs(m - m2) < 1e-9))
        if not same:
            raise SystemExit(f"results and pooled json disagree for {ident}: {(n, m)} vs {(n2, m2)}")
    if set(per) != set(desc):
        raise SystemExit(f"validators differ between results and pooled json: {sorted(per)} vs {sorted(desc)}")
    used = {i: v for i, v in per.items() if v[0] > 0 and v[1] is not None and not math.isnan(v[1])}
    n = sum(v[0] for v in used.values())
    if n == 0:
        raise SystemExit("no usable Arm C window in the pooled Agave stratum: the control cannot be evaluated")
    mean = sum(v[0] * v[1] for v in used.values()) / n
    return {"mean_raw_tau": mean, "usable_windows": n, "per_validator": {i: {"usable": v[0], "mean_tau": v[1]} for i, v in per.items()}}


def evaluate(results: dict, pooled: dict, alloc: dict) -> dict:
    iv, ob = intervals(alloc), observed(results, pooled)
    lo, hi = iv["primary"]["interval"]
    return {"label": "POSITIVE CONTROL (PREREGISTRATION section 7)", "rule": RULE, "arm": ARM, "stratum": POOL_STRATUM,
            "observed": ob, "intervals": iv, "pass": lo <= ob["mean_raw_tau"] <= hi}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--results", required=True)
    ap.add_argument("--pooled", required=True)
    ap.add_argument("--alloc", required=True)
    a = ap.parse_args(argv)
    load = lambda p: json.loads(pathlib.Path(p).read_text(encoding="utf-8"))  # noqa: E731
    res = evaluate(load(a.results), load(a.pooled), load(a.alloc))
    ob, iv = res["observed"], res["intervals"]
    print(f"observed mean raw tau of {RULE}, Arm {ARM}, pooled {POOL_STRATUM}: {ob['mean_raw_tau']:.4f} "
          f"over {ob['usable_windows']} usable windows (registered W = {iv['preset_only']['W']})")
    for k in ("primary", "preset_only", "calibrated", "wifi_sensitivity"):
        lo, hi = iv[k]["interval"]
        print(f"  {k:17s} [{lo:.4f}, {hi:.4f}]  W {iv[k]['W']}")
    print("POSITIVE CONTROL " + ("PASSED" if res["pass"] else
          "*** FAILED *** (reported prominently; weakens every Agave-family verdict, PREREGISTRATION section 7)"))
    return 0 if res["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
