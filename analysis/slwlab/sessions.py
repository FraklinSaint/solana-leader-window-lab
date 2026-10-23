"""PLANNING: hours needed to fill the allocation, and a time-constrained session plan.

``python -m slwlab.sessions`` (run from analysis/; ``make sessions``). Writes
``sim/results/sessions_v1.json`` and ``docs/SESSIONS.md``.

The sender runs on one Windows PC that is on only in the daytime: 3-4 sessions of at most
8-10 h, on different dates. This module answers two questions from committed files only:

1. How many wall-clock hours does each validator, and each (validator x arm) cell, need to
   fill its quota? Rates are the chosen validators' real leader windows in the saved epoch
   schedules (SNAPSHOT ``data/snapshots/leader_schedule_epoch<E>.json.gz``), grouped exactly
   as the planner groups them (planner.rs ``windows_from_leaders``).
2. What does a session plan achieve, per cell: attempted windows, expected usable windows,
   power (SIMULATION curves in ``alloc_v1.json``) and cost? Two options are shown side by
   side and NOT chosen here (the operator decides, before any confirmatory data):
   A. per-validator quotas (current design: each chosen validator is its own cell);
   B. vanilla Agave pooled at stratum level over more validators (the rare stratum).

Session model (assumptions, each printed in the output):
* The saved schedules are the proxy for future epochs. Leader slots are drawn per epoch with
  stake weights and do not depend on the time of day, so session k replays a disjoint L-hour
  block of the saved schedule. Real future schedules differ by sampling noise.
* The first ``CALIB_H`` hours of session 1 go to calibration, ``calibrate.py`` and the
  ``prereg-v1`` freeze. Confirmatory windows use the rest.
* A fraction ``LOSS`` of windows is lost to gaps, late windows and host stalls (seeded
  thinning). Replace it with the fraction measured in the dry-run chunks.
* Per session, each cell may take ceil(remaining / sessions left), so every cell is spread
  over the sessions and their UTC hour ranges (hour of day is not confounded with arm or
  validator). Within a session, windows are taken in time order, >= 5 s apart (planner
  ``min_spacing_ms``), arm = largest remaining share of the session quota (planner quota
  mode), Arm C first on ties (the Agave positive control).
* Power is read at the largest simulated W not above the expected usable count (floor on the
  W grid, no interpolation). Below the smallest grid W it is shown as "<" the W=10 value.
"""

from __future__ import annotations

import argparse
import gzip
import json
import math
import pathlib
import random

SPACING_NS = 5_000_000_000      # planner.rs PlanConfig::default().min_spacing_ms
HOUR_NS = 3_600 * 10**9
CALIB_H = 2.0                   # calibration + calibrate.py + prereg freeze, in session 1
LOSS = 0.15                     # windows lost to gaps/late/stalls (assumption)
SEED = 20261022
FAMILY = {"Agave": "agave", "JitoAgave": "agave", "HarmonicAgave": "agave", "BamAgave": "agave",
          "Rakurai": "agave", "Firedancer": "fd2", "HarmonicFiredancer": "fd2", "BamFiredancer": "fd2"}
ARM_ORDER = ["C", "X", "F", "P", "R"]  # tie order: positive control first
# UTC hour ranges (Kyiv daytime: UTC+3 until 2026-10-25, then UTC+2), spread over the day.
RANGES = {6: [(6, 12), (9, 15), (12, 18), (7, 13)], 8: [(5, 13), (8, 16), (11, 19), (6, 14)],
          10: [(5, 15), (7, 17), (9, 19), (6, 16)]}
SCENARIOS = [(3, 6), (4, 6), (3, 8), (3, 10), (4, 8), (4, 10)]
LOSSES = [LOSS, 0.30]  # base and pessimistic
POOL = "Agave(pooled)"


# ---------------------------------------------------------------- inputs

def mainnet_anchors(data: pathlib.Path) -> list[tuple[int, float]]:
    """(slot, unix_ns): epoch_info snapshots + the middle notification of mainnet recordings."""
    pts = []
    for p in sorted((data / "snapshots").glob("epoch_info_*.json")):
        d = json.loads(p.read_text(encoding="utf-8"))
        if d.get("cluster") == "mainnet":
            pts.append((d["absolute_slot"], (int(d["request_unix_ns"]) + int(d["response_unix_ns"])) / 2))
    recs = []
    for d in sorted((data / "logs").glob("dryrun-*")):
        try:
            if json.loads((d / "run.json").read_text(encoding="utf-8")).get("cluster") == "mainnet":
                recs.append(d / "slots.jsonl")
        except (OSError, ValueError):
            pass
    recs += sorted((data / "exploratory" / "clock_eval").glob("*slots*.jsonl"))
    for p in recs:
        if p.exists():
            obs = [json.loads(l) for l in p.read_text(encoding="utf-8").splitlines() if l.strip()]
            if len(obs) >= 100:
                m = obs[len(obs) // 2]
                pts.append((m["slot"], m["t_ns"]))
    return sorted(pts)


def ns_per_slot(data: pathlib.Path) -> float:
    pts = mainnet_anchors(data)
    if len(pts) < 2 or pts[-1][0] == pts[0][0]:
        raise SystemExit("need two slot/time anchors far apart (run `sender leader-snapshots`)")
    (s0, t0), (s1, t1) = pts[0], pts[-1]
    return (t1 - t0) / (s1 - s0)


def load_epochs(data: pathlib.Path) -> list[dict]:
    out = []
    for p in sorted((data / "snapshots").glob("leader_schedule_epoch*.json.gz")):
        with gzip.open(p, "rt", encoding="utf-8") as f:
            d = json.load(f)
        if d.get("cluster") != "mainnet":
            continue
        leader: list[str | None] = [None] * d["slots_in_epoch"]
        for ident, idx in d["schedule"].items():
            for i in idx:
                leader[i] = ident
        out.append({"epoch": d["epoch"], "first_slot": d["first_slot"], "slots": d["slots_in_epoch"],
                    "leader": leader})
    if not out:
        raise SystemExit("no leader schedule snapshot (run `sender leader-snapshots`)")
    out.sort(key=lambda e: e["epoch"])
    for a, b in zip(out, out[1:]):
        if b["first_slot"] != a["first_slot"] + a["slots"]:
            raise SystemExit(f"saved epochs {a['epoch']} and {b['epoch']} are not consecutive")
    return out


def windows(epoch: dict) -> list[tuple[int, int, str]]:
    """planner.rs windows_from_leaders over one epoch: (first_slot, n_slots, leader)."""
    out: list[list] = []
    for i, l in enumerate(epoch["leader"]):
        if out and out[-1][2] == l:
            out[-1][1] += 1
        else:
            out.append([epoch["first_slot"] + i, 1, l])
    return [(f, n, l) for f, n, l in out if l is not None]


# ---------------------------------------------------------------- session simulation

def pick_arm(quota: dict[str, int], done: dict[str, int]) -> str | None:
    best, share = None, 0.0
    for a in ARM_ORDER:
        q = quota.get(a, 0)
        if q <= 0:
            continue
        r = (q - done.get(a, 0)) / q
        if r > share + 1e-12:
            best, share = a, r
    return best


def run_sessions(stream: list[tuple[float, str]], groups: dict[str, list[str]],
                 quota: dict[str, dict[str, int]], blocks: list[tuple[float, float]],
                 loss: float, seed: int) -> dict:
    """Replay ``stream`` (time ns, leader) over the session ``blocks``. ``groups``: cell owner ->
    identities whose windows count for it (one identity in option A, the pooled stratum in
    option B). ``quota``: owner -> arm -> attempted windows over all sessions."""
    rng = random.Random(seed)
    owner_of = {i: o for o, ids in groups.items() for i in ids}
    done = {o: {a: 0 for a in q} for o, q in quota.items()}
    by_identity: dict[str, int] = {}
    spacing_lost = 0
    per_session = []
    for k, (b0, b1) in enumerate(blocks):
        left = len(blocks) - k
        cap = {o: {a: math.ceil((q[a] - done[o][a]) / left) for a in q} for o, q in quota.items()}
        got = {o: {a: 0 for a in q} for o, q in quota.items()}
        last = None
        for t, ident in stream:
            if t < b0 or t >= b1 or ident not in owner_of:
                continue
            o = owner_of[ident]
            arm = pick_arm(cap[o], got[o])
            if arm is None:
                continue
            if last is not None and t - last < SPACING_NS:
                spacing_lost += 1
                continue
            if rng.random() < loss:
                continue          # lost window (gap, late, stall): no quota used, no fee
            last = t
            got[o][arm] += 1
            by_identity[ident] = by_identity.get(ident, 0) + 1
        for o in got:
            for a in got[o]:
                done[o][a] += got[o][a]
        per_session.append({o: sum(v.values()) for o, v in got.items()})
    return {"done": done, "by_identity": by_identity, "spacing_lost": spacing_lost,
            "per_session": per_session}


def power_at(curves: dict, family: str, arm: str, usable: float) -> dict:
    c = curves.get(f"{family}/{arm}")
    if c is None:
        return {"w": None, "half": None, "exact": None, "below_grid": False}
    grid = sorted(int(w) for w in c["power"])
    fit = [w for w in grid if w <= usable]
    w = fit[-1] if fit else grid[0]
    p = c["power"][str(w)]
    return {"w": w, "half": p["half"], "exact": p["exact"], "below_grid": not fit}


def cell_rows(owner: str, stratum: str, done: dict[str, int], quota: dict[str, int],
              usable_rate: float, curves: dict, spend: dict) -> list[dict]:
    rows = []
    for arm, q in quota.items():
        att = done[arm]
        usable = att * usable_rate
        rows.append({"owner": owner, "stratum": stratum, "arm": arm, "quota": q, "attempted": att,
                     "usable": round(usable, 1), "power": power_at(curves, FAMILY[stratum], arm, usable),
                     "cost": round(att * spend[arm])})
    return rows


# ---------------------------------------------------------------- main computation

def run(data: pathlib.Path, sel_path: pathlib.Path, alloc_path: pathlib.Path,
        loss: float = LOSS, calib_h: float = CALIB_H, extra_agave: int = 4) -> dict:
    sel = json.loads(sel_path.read_text(encoding="utf-8"))
    alloc_doc = json.loads(alloc_path.read_text(encoding="utf-8"))
    alloc = alloc_doc["allocation"]
    pq, usable_rate = alloc["plan_quota"], alloc["usable_rate"]
    spend, curves = alloc["spend_per_window_lamports"], alloc_doc["curves"]
    b = ns_per_slot(data)
    epochs = load_epochs(data)
    t0_slot = epochs[0]["first_slot"]
    span_h = sum(e["slots"] for e in epochs) * b / HOUR_NS

    picked = [p for p in sel["picked"] if p["stratum"] in pq]
    stratum_of = {p["identity"]: p["stratum"] for p in picked}
    notes = [f"{s}: quota in the allocation but no validator in `{sel_path.name}` "
             f"({sum(pq[s].values())} windows per validator cannot be filled)"
             for s in sorted(set(pq) - {p['stratum'] for p in sel['picked']})]

    counts: dict[str, int] = {}
    stream: list[tuple[float, str]] = []
    for e in epochs:
        for f, _, l in windows(e):
            counts[l] = counts.get(l, 0) + 1
            stream.append(((f - t0_slot) * b, l))
    stream.sort()
    wph = {i: n / span_h for i, n in counts.items()}

    vals = []
    for p in picked:
        q, r = pq[p["stratum"]], wph.get(p["identity"], 0.0)
        vals.append({"identity": p["identity"], "stratum": p["stratum"], "version": p.get("version"),
                     "windows_per_hour": r, "quota": sum(q.values()),
                     "hours_validator": sum(q.values()) / r if r else math.inf,
                     "hours_cell": {a: (n / r if r else math.inf) for a, n in q.items()}})
    pool_cands = sorted((c for c in sel["candidates"] if c["stratum"] == "Agave" and c["identity"] not in stratum_of),
                        key=lambda c: (-wph.get(c["identity"], 0.0), c["identity"]))
    ag = [p["identity"] for p in picked if p["stratum"] == "Agave"]
    base_ver = {p.get("version") for p in picked if p["stratum"] == "Agave"}
    extra = [c["identity"] for c in pool_cands if c.get("version") in base_ver][:extra_agave]

    def min_hours(groups: dict[str, list[str]], quota: dict[str, dict[str, int]], loss_: float) -> float | None:
        """Continuous confirmatory hours until every quota is met (one block, no per-session caps)."""
        hi = span_h - calib_h
        if run_sessions(stream, groups, quota, [(calib_h * HOUR_NS, (calib_h + hi) * HOUR_NS)], loss_, SEED)["done"] != quota:
            return None
        lo = 0.0
        while hi - lo > 0.05:
            mid = (lo + hi) / 2
            r_ = run_sessions(stream, groups, quota, [(calib_h * HOUR_NS, (calib_h + mid) * HOUR_NS)], loss_, SEED)
            lo, hi = (lo, mid) if r_["done"] == quota else (mid, hi)
        return round(hi, 1)

    scenarios = []
    losses = sorted({loss, *LOSSES[1:]})   # the chosen loss and the pessimistic one
    for (S, L), lf in [(sl, x) for x in losses for sl in SCENARIOS]:
        if S * L > span_h:
            continue
        blocks = [(k * L * HOUR_NS + (calib_h * HOUR_NS if k == 0 else 0), (k + 1) * L * HOUR_NS)
                  for k in range(S)]
        # A: one cell owner per chosen validator
        qa = {i: dict(pq[s]) for i, s in stratum_of.items()}
        ra = run_sessions(stream, {i: [i] for i in stratum_of}, qa, blocks, lf, SEED)
        rows_a = [r for i, s in stratum_of.items() for r in
                  cell_rows(i, s, ra["done"][i], qa[i], usable_rate, curves, spend)]
        # B: vanilla Agave pooled (the chosen ones + `extra_agave` more on their version, most
        # windows first) with the same total quota; every other validator as in A.
        groups_b = {i: [i] for i, s in stratum_of.items() if s != "Agave"}
        groups_b[POOL] = ag + extra
        qb = {i: dict(pq[stratum_of[i]]) for i in groups_b if i != POOL}
        qb[POOL] = {a: n * len(ag) for a, n in pq["Agave"].items()}
        rb = run_sessions(stream, groups_b, qb, blocks, lf, SEED)
        rows_b = [r for o in groups_b for r in
                  cell_rows(o, "Agave" if o == POOL else stratum_of[o], rb["done"][o], qb[o],
                            usable_rate, curves, spend)]
        scenarios.append({
            "sessions": S, "hours_per_session": L, "utc_ranges": RANGES[L][:S], "loss": lf,
            "confirmatory_hours": S * L - calib_h,
            "A": {"cells": rows_a, "spacing_lost": ra["spacing_lost"], "per_session": ra["per_session"],
                  "total_cost": sum(r["cost"] for r in rows_a),
                  "short": [f"{r['owner'][:8]}/{r['arm']}" for r in rows_a if r["attempted"] < r["quota"]]},
            "B": {"cells": rows_b, "pooled_validators": ag + extra,
                  "pooled_by_validator": {i: rb["by_identity"].get(i, 0) for i in ag + extra},
                  "total_cost": sum(r["cost"] for r in rows_b),
                  "short": [f"{r['owner'][:8]}/{r['arm']}" for r in rows_b if r["attempted"] < r["quota"]]},
        })
    ga = {i: [i] for i in stratum_of}
    qa_all = {i: dict(pq[s]) for i, s in stratum_of.items()}
    gb = {i: [i] for i, s in stratum_of.items() if s != "Agave"} | {POOL: ag + extra}
    qb_all = {i: dict(pq[stratum_of[i]]) for i in gb if i != POOL} | {POOL: {a: n * len(ag) for a, n in pq["Agave"].items()}}
    min_h = [{"loss": x, "A": min_hours(ga, qa_all, x), "B": min_hours(gb, qb_all, x)} for x in losses]
    return {
        "label": "PLANNING",
        "min_continuous_hours": min_h,
        "inputs": {"selection": sel_path.name, "allocation": alloc_path.name,
                   "epochs": [e["epoch"] for e in epochs], "snapshot_hours": round(span_h, 2),
                   "ms_per_slot": round(b / 1e6, 3)},
        "assumptions": {"calibration_hours_in_session_1": calib_h, "lost_fraction": loss, "seed": SEED,
                        "usable_rate": usable_rate, "spacing_s": SPACING_NS / 1e9},
        "notes": notes, "validators": vals,
        "agave_candidates": [{"identity": c["identity"], "version": c.get("version"),
                              "confidence": c.get("confidence"), "windows_per_hour": wph.get(c["identity"], 0.0)}
                             for c in pool_cands],
        "budget": {"confirmatory": alloc["confirmatory_budget_lamports"],
                   "planned": alloc["confirmatory_total_lamports"]},
        "scenarios": scenarios,
    }


# ---------------------------------------------------------------- markdown

def _h(x: float | None) -> str:
    return "—" if x is None or math.isinf(x) else f"{x:.1f}"


def _pw(p: dict) -> str:
    if p["w"] is None:
        return "—"
    if p["below_grid"]:
        return f"<{p['half']:.2f} / <{p['exact']:.2f} (W<{p['w']})"
    return f"{p['half']:.2f} / {p['exact']:.2f} (W≥{p['w']})"


def _cells_table(rows: list[dict]) -> list[str]:
    L = ["| cell | arm | quota (attempted) | attempted | usable (exp.) | power half / exact | cost (lamports) |",
         "|---|---|---|---|---|---|---|"]
    for r in rows:
        name = r["owner"] if r["owner"] == POOL else f"{r['stratum']} `{r['owner'][:8]}`"
        flag = " **short**" if r["attempted"] < r["quota"] else ""
        L.append(f"| {name} | {r['arm']} | {r['quota']} | {r['attempted']}{flag} | {r['usable']} | "
                 f"{_pw(r['power'])} | {r['cost']:,} |")
    return L


def markdown(r: dict) -> str:
    i, a = r["inputs"], r["assumptions"]
    L = ["# Session plan (generated by `python -m slwlab.sessions`, do not edit)", "",
         "> **PLANNING**, not measurement. Leader schedules are SNAPSHOT data; power is SIMULATION "
         f"(`{i['allocation']}` curves, N = 6, half-strength / exact rule). Validators from "
         f"`{i['selection']}`.", "",
         f"Schedule proxy: epochs {', '.join(map(str, i['epochs']))} ({i['snapshot_hours']} h at "
         f"{i['ms_per_slot']} ms/slot). Assumptions: calibration + prereg freeze take the first "
         f"{a['calibration_hours_in_session_1']:.0f} h of session 1; {a['lost_fraction']:.0%} of windows are "
         f"lost to gaps/late/stalls (to be replaced by the measured fraction); usable rate "
         f"{a['usable_rate']:.3f}; windows ≥ {a['spacing_s']:.0f} s apart; seed {a['seed']}.", ""]
    if r["notes"]:
        L += ["**Notes:**", ""] + [f"- {n}" for n in r["notes"]] + [""]
    L += ["## 1. Hours needed per validator and per cell", "",
          "Windows/h = the validator's leader windows in the saved epochs / their duration. "
          "Validator hours = all its cells' quotas / windows per hour; cell hours = that cell's quota "
          "alone. Arms share one validator's windows, so a validator needs the validator hours, not its "
          "largest cell. No gaps or spacing conflicts here (see the scenarios).", "",
          "| stratum | validator | windows/h | quota | validator hours | P | X | R | C | F |",
          "|---|---|---|---|---|---|---|---|---|---|"]
    for v in sorted(r["validators"], key=lambda v: -v["hours_validator"]):
        hc = v["hours_cell"]
        L.append(f"| {v['stratum']} | `{v['identity'][:8]}` | {v['windows_per_hour']:.2f} | {v['quota']} | "
                 f"**{_h(v['hours_validator'])}** | " + " | ".join(_h(hc[x]) if x in hc else "—" for x in "PXRCF")
                 + " |")
    L += ["", "## 2. Time-constrained scenarios", "",
          "Each cell's quota is split over the sessions (ceil(remaining / sessions left)), so every "
          "cell has windows from every session's UTC hours.", "",
          "Minimum continuous confirmatory hours until every quota is met (one block, after "
          "calibration): " + "; ".join(f"loss {m['loss']:.0%}: A {_h(m['A'])} h, B {_h(m['B'])} h"
                                       for m in r["min_continuous_hours"]) + ".", "",
          "| loss | sessions × hours | UTC ranges | confirmatory h | A: cost | A: short cells | B: cost | B: short cells |",
          "|---|---|---|---|---|---|---|---|"]
    for s in r["scenarios"]:
        rng = ", ".join(f"{x:02d}–{y:02d}" for x, y in s["utc_ranges"])
        L.append(f"| {s['loss']:.0%} | {s['sessions']} × {s['hours_per_session']} | {rng} | {s['confirmatory_hours']:.0f} | "
                 f"{s['A']['total_cost']:,} | {', '.join(s['A']['short']) or 'none'} | "
                 f"{s['B']['total_cost']:,} | {', '.join(s['B']['short']) or 'none'} |")
    for s in r["scenarios"]:
        if s["loss"] != r["assumptions"]["lost_fraction"] or (s["A"]["short"] == [] and s["B"]["short"] == []
                                                             and (s["sessions"], s["hours_per_session"]) != (3, 8)):
            continue  # detail: the base loss, the 3 x 8 h plan, and any scenario with a short cell
        rng = ", ".join(f"{x:02d}–{y:02d}" for x, y in s["utc_ranges"])
        L += ["", f"### {s['sessions']} sessions × {s['hours_per_session']} h (UTC {rng}, loss {s['loss']:.0%})", "",
              "**Option A: per-validator quotas (current design)**", ""] + _cells_table(s["A"]["cells"])
        L += ["", "**Option B: vanilla Agave pooled at stratum level** (other strata identical to A). Pooled "
              "windows by validator: " + ", ".join(f"`{v[:8]}` {n}" for v, n in s["B"]["pooled_by_validator"].items()),
              "", "Pooled power is for a stratum-level claim and holds only if every pooled validator "
              "follows the same rule. It is not comparable to a per-validator power in A. The per-validator "
              "counts above are what is left for any per-validator statement.",
              ""] + _cells_table([c for c in s["B"]["cells"] if c["owner"] == POOL])
    L += ["", "## Vanilla Agave candidates (pool for option B)", "",
          "| validator | version | label confidence | windows/h |", "|---|---|---|---|"]
    L += [f"| `{c['identity'][:8]}` | {c['version']} | {c['confidence']} | {c['windows_per_hour']:.2f} |"
          for c in r["agave_candidates"]]
    return "\n".join(L) + "\n"


def main(argv: list[str] | None = None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--data", default="../data")
    ap.add_argument("--selection", help="default: newest data/snapshots/selection_*.json")
    ap.add_argument("--alloc", default="sim/results/alloc_v1.json")
    ap.add_argument("--json", default="sim/results/sessions_v1.json")
    ap.add_argument("--md", default="../docs/SESSIONS.md")
    ap.add_argument("--loss", type=float, default=LOSS)
    a = ap.parse_args(argv)
    data = pathlib.Path(a.data)
    sel = pathlib.Path(a.selection) if a.selection else sorted((data / "snapshots").glob("selection_*.json"))[-1]
    r = run(data, sel, pathlib.Path(a.alloc), loss=a.loss)
    pathlib.Path(a.json).write_text(json.dumps(r, indent=1, sort_keys=True) + "\n", encoding="utf-8", newline="\n")
    pathlib.Path(a.md).write_text(markdown(r), encoding="utf-8", newline="\n")
    print(f"wrote {a.md} and {a.json}")


if __name__ == "__main__":
    main()
