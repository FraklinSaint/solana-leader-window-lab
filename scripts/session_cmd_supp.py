#!/usr/bin/env python3
"""Cap and payer target for the SUPPLEMENTARY session (AMENDMENT 1). Read-only, stdlib only.

    .\\.venv\\Scripts\\python.exe -I scripts\\session_cmd_supp.py --hours 7
    .\\.venv\\Scripts\\python.exe -I scripts\\session_cmd_supp.py --table 5.5,7,8,9

`scripts/session_cmd.py` (a tagged file, untouched) names its allocation file
`alloc_calibrated_session{K}of4.json` and reads the newest selection. This file reuses its
functions (`attempted`, `committed`, constants) and its formula

    remaining = (plan_quota of the file - windows already attempted) x fee per window,
                over the validators of the selection copy (EvnRmnMr is not in it)
    cap       = ceil(remaining x 1.05 x HOURS / HOURS_AVAILABLE)      HOURS_AVAILABLE default HOURS + 0.5

with an explicit allocation file and selection file (defaults: AMENDMENT 1 full-cap file and the
selection copy without EvnRmnMr). `plan_quota` is cumulative (4of4 quota + supplementary), so the
39 windows of 4of4 still unplanned are inside `remaining`.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import math
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("session_cmd", ROOT / "scripts" / "session_cmd.py")
sc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sc)

ALLOC = ROOT / "analysis" / "sim" / "results" / "sessions" / "alloc_supplementary_fullcap.json"
SELECTION = ROOT / "analysis" / "sim" / "results" / "sessions" / "selection_amendment1_no_EvnRmnMr.json"


def compute(data: pathlib.Path, alloc_path: pathlib.Path, sel_path: pathlib.Path, hours: float,
            hours_available: float | None) -> dict:
    avail = hours + 0.5 if hours_available is None else hours_available
    alloc = json.loads(alloc_path.read_text(encoding="utf-8"))
    quota = alloc["allocation"]["plan_quota"]
    spend = alloc["allocation"]["spend_per_window_lamports"]
    sel = json.loads(sel_path.read_text(encoding="utf-8"))
    stratum_of = {p["identity"]: p["stratum"] for p in sel["picked"]}
    done = sc.attempted(data)
    windows = cost = 0
    by_val: dict[str, int] = {}
    for ident, s in stratum_of.items():
        for arm, q in quota.get(s, {}).items():
            left = max(q - done.get((ident, arm), 0), 0)
            windows += left
            cost += left * spend[arm]
            by_val[ident[:8]] = by_val.get(ident[:8], 0) + left
    cap = math.ceil(cost * sc.MARGIN * hours / avail)
    return {"hours": hours, "avail": avail, "windows": windows, "cost": cost, "cap": cap,
            "target": sc.RENT_MIN + math.ceil(cap / sc.PAYERS) + sc.WORST_TX_FEE, "by_validator": by_val,
            "committed": sc.committed(data), "validators": len(stratum_of)}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--hours", type=float, default=7.0)
    ap.add_argument("--hours-available", type=float, default=None, help="default HOURS + 0.5")
    ap.add_argument("--alloc", default=str(ALLOC))
    ap.add_argument("--selection", default=str(SELECTION))
    ap.add_argument("--data", default=str(ROOT / "data"))
    ap.add_argument("--table", help="comma list of hours: cap and payer target for each (HA = H + 0.5)")
    ap.add_argument("--print", dest="what", choices=["all", "cap", "target"], default="all")
    a = ap.parse_args(argv)
    data, al, se = pathlib.Path(a.data), pathlib.Path(a.alloc), pathlib.Path(a.selection)
    if a.table:
        print("H     HA    remaining windows  remaining cost   cap (lamports)  payer target")
        for h in (float(x) for x in a.table.split(",")):
            r = compute(data, al, se, h, None)
            print(f"{h:<5g} {r['avail']:<5g} {r['windows']:>17}  {r['cost']:>14,}  {r['cap']:>14,}  {r['target']:>12,}")
        return 0
    if not 0 < a.hours <= 10:
        print("--hours must be in (0, 10]", file=sys.stderr)
        return 2
    r = compute(data, al, se, a.hours, a.hours_available)
    if a.what != "all":
        print(r["cap"] if a.what == "cap" else r["target"])
        return 0
    print(f"supplementary session, {a.hours:g} h (HA {r['avail']:g}), allocation {al.name}, selection {se.name} ({r['validators']} validators)")
    print(f"remaining quota of this file: {r['windows']} windows, {r['cost']:,} lamports; by validator {dict(sorted(r['by_validator'].items()))}")
    print(f"cap = ceil({r['cost']:,} x {sc.MARGIN} x {a.hours:g}/{r['avail']:g}) = {r['cap']:,} lamports")
    print(f"payer target: {sc.RENT_MIN:,} + {math.ceil(r['cap'] / sc.PAYERS):,} + {sc.WORST_TX_FEE:,} = {r['target']:,}")
    print(f"confirmatory budget: {r['committed']:,} committed, {sc.CONFIRMATORY_BUDGET - r['committed']:,} left")
    return 0


if __name__ == "__main__":
    sys.exit(main())
