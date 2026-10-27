#!/usr/bin/env python3
"""Exact command for confirmatory session K of 4 (RUNBOOK_MAINNET S7b). Read-only, stdlib only.

    .\\.venv\\Scripts\\python.exe -I scripts\\session_cmd.py --k 1 --hours 8

The sender paces spend along a straight line from 0 to the session cap over the session's hours
(`planner::Pace`). The cap must therefore match what the session has to do, otherwise the plan fills
early and the last hours stay idle (a cap of 9.5M for a quarter plan of about 7.5M would end the
session after about 6 h). This script computes it from the allocation file and the ledger:

    remaining = (cumulative quota of the session's file - windows already attempted) x fee per window
    cap       = ceil(remaining x 1.05 x HOURS / 8)

`windows already attempted` is counted like `runner::prior_plan_state`: the spend ledger's
`measure` reservations joined with each run's `plan.jsonl` (identity and arm), once per window.
Earlier sessions' shortfalls are therefore included. A session shorter than 8 hours gets a
proportionally smaller cap, and what it leaves undone carries over to the next session's file.
It also prints the payer target for that cap (README "Payer sizing").
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
MARGIN = 1.05
BASE_HOURS = 8
PAYERS = 32
RENT_MIN = 650_240        # rent minimum for 0 bytes (README); the fund dry run prints the live value
WORST_TX_FEE = 6_600      # 5,000 + 1,600 (README)
CONFIRMATORY_BUDGET = 38_000_000


def read_jsonl(p: pathlib.Path) -> list[dict]:
    if not p.exists():
        return []
    out = []
    for l in p.read_text(encoding="utf-8").splitlines():
        if l.strip():
            try:
                out.append(json.loads(l))
            except json.JSONDecodeError:
                pass  # a torn last line, as in the sender
    return out


def newest_selection(data: pathlib.Path) -> pathlib.Path:
    return sorted((data / "snapshots").glob("selection_*.json"))[-1]


def alloc_path(repo: pathlib.Path, k: int, sessions: int) -> pathlib.Path:
    return repo / "analysis" / "sim" / "results" / "sessions" / f"alloc_calibrated_session{k}of{sessions}.json"


def attempted(data: pathlib.Path, purpose: str = "measure") -> dict[tuple[str, str], int]:
    """(identity, arm) -> windows attempted so far for `purpose` (ledger x plan.jsonl)."""
    wins: dict[str, set[str]] = {}
    for e in read_jsonl(data / "ledger.jsonl"):
        if e.get("kind") == "reserve" and e.get("purpose") == purpose and e.get("window_id"):
            wins.setdefault(e["run_id"], set()).add(e["window_id"])
    done: dict[tuple[str, str], int] = {}
    for run_id, ids in wins.items():
        plan = {}
        for l in read_jsonl(data / "logs" / run_id / "plan.jsonl"):
            w = l.get("window")
            if w:
                plan[w["window_id"]] = (w["identity"], w["design"]["arm"])
        for wid in ids:
            if wid not in plan:
                raise SystemExit(f"window {wid} has a ledger reservation but is not in logs/{run_id}/plan.jsonl")
            done[plan[wid]] = done.get(plan[wid], 0) + 1
    return done


def committed(data: pathlib.Path, purpose: str = "measure") -> int:
    """Fees paid plus open reservations for `purpose` (as main.rs ledger_commitment)."""
    open_: dict[str, tuple[str, int]] = {}
    used = 0
    for e in read_jsonl(data / "ledger.jsonl"):
        k = e.get("kind")
        if k == "reserve":
            open_[e["sig"]] = (e["purpose"], e["amount"])
        elif k == "landed":
            p = open_.pop(e["sig"], None)
            if p and p[0] == purpose:
                used += e["fee"]
        elif k == "expired":
            open_.pop(e["sig"], None)
    return used + sum(a for p, a in open_.values() if p == purpose)


def compute(repo: pathlib.Path, data: pathlib.Path, k: int, hours: float, sessions: int = 4,
            hours_available: float | None = None) -> dict:
    """`hours_available`: hours in which the remaining quota must be done, over all sessions still to
    run (default BASE_HOURS = 8, the length of one planned session). A session of `hours` then gets
    the share hours / hours_available of the remaining cost."""
    avail = BASE_HOURS if hours_available is None else hours_available
    alloc = json.loads(alloc_path(repo, k, sessions).read_text(encoding="utf-8"))
    quota = alloc["allocation"]["plan_quota"]
    spend = alloc["allocation"]["spend_per_window_lamports"]
    sel = json.loads(newest_selection(data).read_text(encoding="utf-8"))
    stratum_of = {p["identity"]: p["stratum"] for p in sel["picked"]}
    done = attempted(data)
    remaining_windows = remaining_cost = 0
    by_stratum: dict[str, int] = {}
    for ident, s in stratum_of.items():
        for arm, q in quota.get(s, {}).items():
            left = max(q - done.get((ident, arm), 0), 0)
            remaining_windows += left
            remaining_cost += left * spend[arm]
            by_stratum[s] = by_stratum.get(s, 0) + left
    cap = math.ceil(remaining_cost * MARGIN * hours / avail)
    # what this session's own share is: cost of (this file - the previous file); the rest is carry-over
    increment = remaining_cost
    if k > 1:
        prev = json.loads(alloc_path(repo, k - 1, sessions).read_text(encoding="utf-8"))["allocation"]["plan_quota"]
        increment = sum(max(q - prev.get(s, {}).get(arm, 0), 0) * spend[arm]
                        for ident, s in stratum_of.items() for arm, q in quota.get(s, {}).items())
    line_per_hour = cap / hours
    mean_fee = remaining_cost / remaining_windows if remaining_windows else 0
    return {
        "carry_over_cost": round(remaining_cost - increment), "line_per_hour": round(line_per_hour),
        "windows_per_hour": round(line_per_hour / mean_fee, 1) if mean_fee else 0.0,
        "session": k, "sessions": sessions, "hours": hours, "hours_available": avail, "allocation_file": alloc_path(repo, k, sessions),
        "selection": newest_selection(data).name, "windows_done": sum(done.values()),
        "remaining_windows": remaining_windows, "remaining_cost": round(remaining_cost),
        "remaining_by_stratum": by_stratum, "cap": cap,
        "payer_target": RENT_MIN + math.ceil(cap / PAYERS) + WORST_TX_FEE,
        "committed_measure": committed(data), "budget_left": CONFIRMATORY_BUDGET - committed(data),
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--k", type=int, required=True, help="session number 1..4")
    ap.add_argument("--hours", type=float, default=8.0, help="session length, at most 10")
    ap.add_argument("--hours-available", type=float, default=None,
                    help="hours left for ALL remaining sessions of the quota (default 8): cap = remaining cost x "
                         "1.05 x HOURS / this. Outcome-blind; chosen from the hours the PC can run.")
    ap.add_argument("--sessions", type=int, default=4)
    ap.add_argument("--data", default=str(ROOT / "data"))
    ap.add_argument("--print", dest="what", choices=["all", "cap", "target"], default="all",
                    help="cap / target: only that integer (for a PowerShell variable)")
    a = ap.parse_args(argv)
    if not 1 <= a.k <= a.sessions or not 0 < a.hours <= 10:
        print("--k must be 1..4 and --hours in (0, 10]", file=sys.stderr)
        return 2
    r = compute(ROOT, pathlib.Path(a.data), a.k, a.hours, a.sessions, a.hours_available)
    if a.what != "all":
        print(r["cap"] if a.what == "cap" else r["payer_target"])
        return 0
    rel = r["allocation_file"].relative_to(ROOT).as_posix().replace("/", "\\")
    print(f"session {a.k} of {a.sessions}, {a.hours:g} h, selection {r['selection']}")
    print(f"windows attempted so far (measure): {r['windows_done']}")
    print(f"remaining quota of this file: {r['remaining_windows']} windows, {r['remaining_cost']:,} lamports  {r['remaining_by_stratum']}")
    print(f"  of which carried over from earlier sessions: {r['carry_over_cost']:,} lamports")
    print(f"cap = ceil({r['remaining_cost']:,} x {MARGIN} x {a.hours:g}/{r['hours_available']:g}) = {r['cap']:,} lamports")
    print(f"payer target for this cap: {RENT_MIN:,} + {math.ceil(r['cap'] / PAYERS):,} + {WORST_TX_FEE:,} = {r['payer_target']:,}")
    print(f"confirmatory budget: {r['committed_measure']:,} committed, {r['budget_left']:,} left")
    if r["cap"] > r["budget_left"]:
        print("WARNING: the cap is above the confirmatory budget left; the planner will stop at the budget.", file=sys.stderr)
    if r["remaining_windows"] == 0:
        print("WARNING: nothing is left of this file's quota; the session would plan no window.", file=sys.stderr)
    h = r["windows_per_hour"]
    print(f"pacing line: {r['line_per_hour']:,} lamports per hour = about {h:g} windows per hour; "
          + (f"the quota is used up after about {a.hours / MARGIN:.1f} of {a.hours:g} h" if a.hours >= r["hours_available"]
             else f"this session takes {a.hours:g} of the {r['hours_available']:g} h available; the rest stays for the next session"))
    print(f"FIRST-HOUR CHECK (monitor ratio 0.80 to 1.05): reserved {round(0.8 * r['line_per_hour']):,} to "
          f"{round(1.05 * r['line_per_hour']):,} lamports, about {math.floor(0.8 * h)} to {math.ceil(1.05 * h)} windows, "
          f"every 15-min bin at least {max(math.floor(h / 4 / 2.5), 1)} (expected about {h / 4:.1f})")
    print("\nPowerShell:")
    print(f"& $S fund --payer-target-lamports {r['payer_target']}")
    print(f"$env:CONFIRM_LIVE = 'yes'")
    print(f"& $S fund --payer-target-lamports {r['payer_target']} --live --max-spend-lamports 200000")
    print(f"Remove-Item Env:CONFIRM_LIVE")
    print('git add data')
    print("git commit -m \"fund top-up $((Get-Date).ToUniversalTime().ToString('yyyy-MM-dd HH:mm'))Z\"")
    print(f"$env:CONFIRM_LIVE = 'yes'")
    print(f"& $S session --mode measure --hours {a.hours:g} --max-spend-lamports {r['cap']} --prereg $PRE --allocation {rel}")
    print(f"Remove-Item Env:CONFIRM_LIVE")
    return 0


if __name__ == "__main__":
    sys.exit(main())
