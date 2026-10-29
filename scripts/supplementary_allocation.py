#!/usr/bin/env python3
"""Supplementary allocation (AMENDMENT 1 of 2026-10-29, docs/AMENDMENT_1_SUPPLEMENTARY.md; read-only, sends nothing).

    python scripts/supplementary_allocation.py --write      # allocation files + dry run
    python scripts/supplementary_allocation.py --check      # files on disk == fresh build

It reads ONLY per-cell COUNTS: for every validator x arm (X also by gap) the number of usable
windows U (PREREGISTRATION §9, stored getBlocks lists) and of attempted windows A of purpose
`measure`, plus the registered targets T (`W_usable_per_validator` x validators of alloc_v2.json,
scenario calibrated) and the planned attempted quota Q of `alloc_calibrated_session4of4.json`.
It never computes or reads tau, a p-value, a rank, a verdict or an arm comparison. It refuses to
run unless the newest `data/verify/verify_*.json` is ok and covers every manifest line.

Rules (docs/PROPOSAL_SUPPLEMENTARY_AMENDMENT.md, 2026-10-29):
 1. quota of a cell, in attempted windows: min(cap, max(0, T - U)); pooled Agave pools its two
    validators as registered.
 2. cap per cell = n_validators_in_cell x S per validator, S = ceil(Q / 2) ("half") or Q ("full").
 3. supplementary X windows only at gaps 0, 10 and 25 ms, split evenly (the remainder goes to the
    lowest gaps first).
 4. EvnRmnMr is excluded (0 usable of 75 attempted, counts only); the Firedancer cells are
    measured by 9eGrDohd alone (n_validators 1, T unchanged, see the sensitivity in the dry run).
The allocation file has the format of alloc_calibrated_session{K}of4.json; its plan_quota is
CUMULATIVE (the 4of4 quota plus the supplementary quota per validator), because the planner asks
for `quota - done` per validator and arm (`planner.rs` `pick_quota_arm`).
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import math
import pathlib
import sys
from collections import Counter, defaultdict

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "analysis"))

from slwlab import blocklist, parse  # noqa: E402

EXCLUDED = {"EvnRmnMr": "0 usable of 75 attempted windows (counts), every send fails at the connection"}
X_GAPS = (0, 10, 25)
ARMS = "PFXRC"
CAPS = ("half", "full")
HOURS_PER_NIGHT = 5.5
SENDER_RATE = 44.0  # windows an hour (session 4: 239 windows in 5.5 h)
CONFIRMATORY_BUDGET = 38_000_000
SELECTION = "selection_20261022T163227Z.json"
SELECTION_COPY = "analysis/sim/results/sessions/selection_amendment1_no_EvnRmnMr.json"
SELECTION_DEPLOYED = "data/snapshots/selection_20261029T112100Z_amendment1_no_EvnRmnMr.json"


def sha256_file(p: pathlib.Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def dump(doc: dict) -> bytes:
    return (json.dumps(doc, indent=1, sort_keys=True) + "\n").encode("utf-8")


def v8(identity: str) -> str:
    return identity[:8]


# ----------------------------------------------------------------------------- counts

def load_counts(data: pathlib.Path) -> dict:
    """Per-window tuples reduced to counts. Nothing else leaves this function."""
    verify = sorted((data / "verify").glob("verify_*.json"))
    if not verify:
        raise SystemExit("no data/verify/verify_*.json: run scripts/verify_manifest.py first")
    v = json.loads(verify[-1].read_text(encoding="utf-8"))
    n_lines = 0
    for m in (data / "manifest.jsonl", data / "exploratory" / "calibration_manifest.jsonl"):
        if m.exists():
            n_lines += sum(1 for line in m.read_text(encoding="utf-8").splitlines() if line.strip())
    if not v.get("ok") or v.get("problems") or v["summary"]["offline"]["txs"] != n_lines:
        raise SystemExit(f"{verify[-1].name} is not ok or does not cover the manifests ({n_lines} lines)")
    lines = [json.loads(line) for line in (data / "manifest.jsonl").read_text(encoding="utf-8").splitlines() if line.strip()]
    meas = [x for x in lines if x["tx"]["purpose"] == "measure"]
    runs = sorted({x["tx"]["run_id"] for x in meas})
    windows = parse.windows_from_manifest(meas, blocklist.present_map(data, runs))
    paid = sum((x["outcome"].get("fee_paid") or 0) for x in meas)
    rows = [(w.stratum, v8(w.identity), w.arm, w.gap_us // 1000, bool(w.usable)) for w in windows]
    return {"rows": rows, "measure_fees_paid": paid, "verify_file": verify[-1].name, "windows": len(windows)}


def cell_counts(rows: list[tuple]) -> dict:
    cells: dict = defaultdict(lambda: {"A": 0, "U": 0, "val": defaultdict(lambda: [0, 0]), "gap": defaultdict(lambda: [0, 0])})
    for stratum, val, arm, gap, usable in rows:
        if val in EXCLUDED:
            c = cells[("EXCLUDED", arm)]
            c["A"] += 1
            c["U"] += usable
            continue
        c = cells[(stratum, arm)]
        c["A"] += 1
        c["U"] += usable
        c["val"][val][0] += 1
        c["val"][val][1] += usable
        if arm == "X":
            c["gap"][gap][0] += 1
            c["gap"][gap][1] += usable
    return cells


# ----------------------------------------------------------------------------- supply

def supply(data: pathlib.Path) -> dict:
    """Leader windows an hour per validator (8-char prefix) from the saved mainnet schedules."""
    from slwlab import sessions
    ns = sessions.ns_per_slot(data)
    per = defaultdict(lambda: [0, 0.0])
    epochs = []
    for p in sorted((data / "snapshots").glob("leader_schedule_epoch*.json.gz")):
        with gzip.open(p, "rt", encoding="utf-8") as f:
            d = json.load(f)
        if d.get("cluster") != "mainnet":
            continue
        leader = [None] * d["slots_in_epoch"]
        for ident, idx in d["schedule"].items():
            for i in idx:
                leader[i] = ident
        hours = d["slots_in_epoch"] * ns / 1e9 / 3600
        runs = Counter()
        prev = None
        for l in leader:
            if l is not None and l != prev:
                runs[v8(l)] += 1
            prev = l
        epochs.append((d["epoch"], hours, runs))
    for _, hours, runs in epochs:
        for val, n in runs.items():
            per[val][0] += n
            per[val][1] += hours
    return {val: n / h for val, (n, h) in per.items() if h}, [e[0] for e in epochs]


# ----------------------------------------------------------------------------- allocation

def targets(alloc: dict) -> dict:
    rows = alloc["scenarios"]["calibrated"]["allocation"]["rows"]
    return {(r["group"], r["arm"]): (r["W_usable_per_validator"], r["validators"]) for r in rows}


def n_eff(stratum: str, validators_in_cell: int) -> int:
    return 1 if stratum == "Firedancer" else validators_in_cell


def allocation(counts: dict, tgt: dict, quota4: dict, cap: str, fd_target: str = "registered") -> list[dict]:
    cells = cell_counts(counts["rows"])
    out = []
    for stratum in sorted(quota4):
        for arm in ARMS:
            if arm not in quota4[stratum]:
                continue
            w, nv_reg = tgt[(stratum, arm)]
            T = w * nv_reg
            ne = n_eff(stratum, nv_reg)
            if stratum == "Firedancer" and fd_target == "per_validator":
                T = w * ne
            c = cells[(stratum, arm)]
            Q = quota4[stratum][arm]
            S = math.ceil(Q / 2) if cap == "half" else Q
            deficit = max(0, T - c["U"])
            q = min(ne * S, deficit)
            qv = math.ceil(q / ne) if q else 0
            rec = {"stratum": stratum, "arm": arm, "T": T, "U": c["U"], "A": c["A"], "Q": Q, "S": S, "validators": sorted(c["val"]),
                   "n_validators": ne, "deficit": deficit, "cap_cell": ne * S, "quota": q, "quota_per_validator": qv,
                   "cap_binds": deficit > ne * S}
            if arm == "X":
                base, rem = divmod(qv, len(X_GAPS))
                rec["x_gap_quota_per_validator"] = {str(g): base + (1 if i < rem else 0) for i, g in enumerate(X_GAPS)}
            out.append(rec)
    return out


def build_file(recs: list[dict], quota4_doc: dict, cap: str, counts: dict) -> dict:
    full = quota4_doc["full_plan_quota"]
    plan = {s: dict(a) for s, a in full.items()}
    supp: dict = {}
    xg: dict = {}
    for r in recs:
        if r["quota_per_validator"]:
            plan[r["stratum"]][r["arm"]] = full[r["stratum"]][r["arm"]] + r["quota_per_validator"]
            supp.setdefault(r["stratum"], {})[r["arm"]] = r["quota_per_validator"]
        if r["arm"] == "X" and r["quota_per_validator"]:
            xg[r["stratum"]] = r["x_gap_quota_per_validator"]
    return {
        "label": "SUPPLEMENTARY ALLOCATION FILE (AMENDMENT 1, 2026-10-29; scripts/supplementary_allocation.py; the fullcap file is the one used)",
        "source": {"file": "alloc_calibrated_session4of4.json", "targets": "alloc_v2.json scenario calibrated",
                   "counts": f"usable and attempted windows of purpose measure, sessions 1 to 4, {counts['verify_file']}"},
        "session": "supplementary", "sessions": 4, "cumulative_share": "4/4 + supplementary", "cap": cap,
        "rounding": "plan_quota = full_plan_quota (4of4) + supplementary quota per validator; quota = min(cap, max(0, T - U))",
        "dropped_strata": quota4_doc["dropped_strata"],
        "excluded_validators": sorted(EXCLUDED),
        "allocation": {"n": quota4_doc["allocation"]["n"], "usable_rate": quota4_doc["allocation"]["usable_rate"],
                       "spend_per_window_lamports": quota4_doc["allocation"]["spend_per_window_lamports"], "plan_quota": plan},
        "full_plan_quota": plan,
        "supplementary": {"quota_per_validator": supp, "x_gap_quota_per_validator": xg,
                          "note": "x_gap_quota_per_validator and excluded_validators are NOT read by the sender (planner.rs): "
                                  "X gaps rotate over arm_cfg.x_gaps_ms and every validator of a stratum gets the same quota; "
                                  "see docs/SUPPLEMENTARY_DRYRUN_FULL.txt for what this needs"},
    }


# ----------------------------------------------------------------------------- dry run

def rates(counts: dict) -> dict:
    """Observed usable rates over the validators that stay in (EvnRmnMr out): per arm, X per gap."""
    arm = defaultdict(lambda: [0, 0])
    gap = defaultdict(lambda: [0, 0])
    for stratum, val, a, g, u in counts["rows"]:
        if val in EXCLUDED:
            continue
        arm[a][0] += 1
        arm[a][1] += u
        if a == "X":
            gap[g][0] += 1
            gap[g][1] += u
    return {"arm": {a: v[1] / v[0] for a, v in arm.items()}, "gap": {g: v[1] / v[0] for g, v in gap.items()},
            "arm_n": {a: tuple(v) for a, v in arm.items()}, "gap_n": {g: tuple(v) for g, v in gap.items()}}


def leftovers(counts: dict, quota4: dict) -> dict:
    att = Counter()
    strat = {}
    for stratum, val, arm, gap, u in counts["rows"]:
        att[(val, arm)] += 1
        strat[val] = stratum
    out = {}
    for val, stratum in strat.items():
        if val in EXCLUDED:
            continue
        out[val] = {a: max(0, q - att[(val, a)]) for a, q in quota4[stratum].items()}
    return out


def pct(x: float) -> str:
    return f"{100 * x:.1f}%"


def dryrun_text(counts: dict, tgt: dict, quota4_doc: dict, sup: dict, epochs: list, spend: dict, script_sha: str) -> str:
    quota4 = quota4_doc["full_plan_quota"]
    rt = rates(counts)
    left = leftovers(counts, quota4)
    L: list[str] = []
    A = L.append
    A("SUPPLEMENTARY ALLOCATION, DRY RUN (numbers only; for AMENDMENT 1 of 2026-10-29, cap full is the one used; nothing sent)")
    A(f"generated by scripts/supplementary_allocation.py (SHA-256 {script_sha}); counts from {counts['verify_file']} ({counts['windows']} measure windows, sessions 1 to 4)")
    A("No tau, p-value, rank or verdict is read or computed. Usable = PREREGISTRATION §9 with the stored getBlocks lists.")
    A("")
    A("== 0. Rules applied")
    A("quota (attempted windows) = min(cap, max(0, T - U)); cap = n_validators x S per validator; S half = ceil(Q/2), S full = Q (Q = planned attempted quota per validator, 4of4 file).")
    A("X only at gaps 0, 10, 25 ms (60 ms: 6 of 36 usable; 120 ms: 0 of 36, all sessions); EvnRmnMr excluded (0 usable of 75 attempted); Firedancer cells measured by 9eGrDohd alone (T unchanged, n_validators 1).")
    A(f"Usable rates for 'attempts needed' are observed over the validators that stay in (EvnRmnMr out): per arm and, for X, per gap.")
    A("  per arm (windows, usable, rate): " + "; ".join(f"{a} {rt['arm_n'][a][0]}, {rt['arm_n'][a][1]}, {pct(rt['arm'][a])}" for a in ARMS if a in rt["arm_n"]))
    A("  X per gap (windows, usable, rate): " + "; ".join(f"{g} ms {rt['gap_n'][g][0]}, {rt['gap_n'][g][1]}, {pct(rt['gap'][g])}" for g in sorted(rt["gap_n"])))
    A(f"  supply (leader windows an hour, from saved mainnet epochs {epochs}): " + ", ".join(f"{k} {sup[k]:.2f}" for k in sorted(sup, key=lambda k: -sup[k]) if k in
        {"5Us18hLZ", "7QQGNm3p", "9eGrDohd", "DRpbCBMx", "E1r4Psq8", "Fd7btgyS", "HEL1USMZ", "JUPiTERr", "ana2y2Yv"}))
    A(f"  sender rate {SENDER_RATE:.0f} windows an hour (session 4: 239 in 5.5 h); night = {HOURS_PER_NIGHT} h = {SENDER_RATE * HOURS_PER_NIGHT:.0f} windows at the sender rate.")
    A("")
    A("== 1. Counts per cell (sessions 1 to 4; U attempted A; T registered target; Q planned attempted per validator)")
    base = allocation(counts, tgt, quota4, "full")
    A("stratum             arm   T    U    A   deficit  Q/validator  validators")
    for r in base:
        A(f"{r['stratum']:<19} {r['arm']}  {r['T']:>3}  {r['U']:>3}  {r['A']:>3}  {r['deficit']:>6}  {r['Q']:>8}     {','.join(r['validators'])}")
    A("")
    totals = {}
    for cap in CAPS:
        recs = allocation(counts, tgt, quota4, cap)
        A(f"== 2.{CAPS.index(cap) + 1} Cap = {cap} (S = {'ceil(Q/2)' if cap == 'half' else 'Q'} per validator)")
        A("stratum             arm  deficit  cap(cell)  quota  per-val  cap binds  rate    attempts needed (D/rate)  expected usable at quota   cost lamports")
        tot_q = tot_need = 0
        tot_cost = 0.0
        per_val: Counter = Counter()
        per_val_arm: dict = defaultdict(Counter)
        for r in recs:
            if not r["quota"]:
                continue
            if r["arm"] == "X":
                gq = r["x_gap_quota_per_validator"]
                # expected usable at quota by gap rates; needed = deficit / weighted rate over the gap split
                tq = sum(gq.values())
                wr = sum(gq[str(g)] * rt["gap"][g] for g in X_GAPS) / tq
            else:
                wr = rt["arm"][r["arm"]]
            need = math.ceil(r["deficit"] / wr) if wr else 0
            exp = r["quota"] * wr
            cost = r["quota_per_validator"] * r["n_validators"] * spend[r["arm"]]
            A(f"{r['stratum']:<19} {r['arm']}   {r['deficit']:>5}  {r['cap_cell']:>8}  {r['quota']:>5}  {r['quota_per_validator']:>6}  {'yes' if r['cap_binds'] else 'no':>9}  {pct(wr):>6}   {need:>14}  {exp:>20.1f}   {cost:>14,.0f}")
            if r["arm"] == "X":
                for g in X_GAPS:
                    A(f"    X gap {g:>3} ms: quota per validator {r['x_gap_quota_per_validator'][str(g)]:>2}  (observed usable rate {pct(rt['gap'][g])})")
            tot_q += r["quota"]
            tot_need += need
            tot_cost += cost
            vals = r["validators"] if r["stratum"] != "Firedancer" else ["9eGrDohd"]
            for v in vals:
                per_val[v] += r["quota_per_validator"]
                per_val_arm[v][r["arm"]] += r["quota_per_validator"]
        fut = [f"{r['stratum']} {r['arm']} (A {r['A']}, U {r['U']}, T-U {r['deficit']} > S x U/A = {r['S'] * r['U'] / r['A']:.1f})" for r in recs
               if r["A"] >= 10 and r["deficit"] > r["S"] * r["U"] / r["A"] and r["deficit"] > 0 and r["stratum"] != "Firedancer"]
        A("   info only, not applied: cells the 2026-10-28 draft futility test (A >= 10 and T - U > S x U/A) would drop at this cap: " + ("; ".join(fut) or "none"))
        A(f"TOTAL cap {cap}: quota {tot_q} attempted windows (cells), attempts needed to close at observed rates {tot_need}, cost at quota {tot_cost:,.0f} lamports")
        A("")
        A(f"   per validator, cap {cap}: leftover of 4of4 + supplementary = windows; hours at supply = windows / windows per hour")
        A("   validator   stratum              4of4 left  supplementary  total  supply/h   hours at supply   (supp by arm)")
        rows = []
        for v in sorted(left):
            lv = sum(left[v].values())
            sv = per_val.get(v, 0)
            hrs = (lv + sv) / sup[v]
            strat = next(r["stratum"] for r in recs if v in r["validators"])
            rows.append((hrs, v, strat, lv, sv))
            A(f"   {v:<10}  {strat:<19} {lv:>9}  {sv:>13}  {lv + sv:>5}  {sup[v]:>8.2f}  {hrs:>15.2f}   {dict(sorted(per_val_arm[v].items()))}")
        rows.sort(reverse=True)
        tot_w = sum(r[3] + r[4] for r in rows)
        h_sender = tot_w / SENDER_RATE
        h_sup = rows[0][0]
        hours = max(h_sup, h_sender)
        # cost of the 4of4 leftover at the spend per window of each arm
        left_cost = sum(n * spend[a] for v in left for a, n in left[v].items())
        A(f"   TOTAL windows (supplementary {sum(r[4] for r in rows)} + 4of4 leftover {sum(r[3] for r in rows)}) = {tot_w}")
        A(f"   bottleneck validator: {rows[0][1]} ({rows[0][2]}), {rows[0][0]:.2f} h at supply")
        A(f"   hours at the sender rate ({SENDER_RATE:.0f}/h): {h_sender:.2f} h; hours at supply (bottleneck): {h_sup:.2f} h; binding: {max(h_sup, h_sender):.2f} h")
        A(f"   nights of {HOURS_PER_NIGHT} h: {math.ceil(hours / HOURS_PER_NIGHT)} (hours / {HOURS_PER_NIGHT} = {hours / HOURS_PER_NIGHT:.2f}); at the sender rate alone {h_sender / HOURS_PER_NIGHT:.2f}, at supply alone {h_sup / HOURS_PER_NIGHT:.2f}")
        A(f"   cost: supplementary at quota {tot_cost:,.0f} + 4of4 leftover {left_cost:,.0f} = {tot_cost + left_cost:,.0f} lamports")
        remaining = CONFIRMATORY_BUDGET - counts["measure_fees_paid"]
        A(f"   confirmatory budget {CONFIRMATORY_BUDGET:,} - measure fees paid so far {counts['measure_fees_paid']:,} = {remaining:,} left; the plan uses {100 * (tot_cost + left_cost) / remaining:.1f}% of it")
        A("")
        totals[cap] = (tot_q, tot_cost, left_cost, hours, rows[0][1])
    A("== 3. Sensitivity: Firedancer target per validator (W) instead of the registered pooled 2 x W (9eGrDohd alone)")
    for cap in CAPS:
        s = [r for r in allocation(counts, tgt, quota4, cap, "per_validator") if r["stratum"] == "Firedancer"]
        b = [r for r in allocation(counts, tgt, quota4, cap) if r["stratum"] == "Firedancer"]
        A(f"   cap {cap}: Firedancer quota per validator, registered T vs T = W: " + "; ".join(f"{x['arm']} {y['quota_per_validator']} vs {x['quota_per_validator']} (T {y['T']} vs {x['T']}, U {x['U']})" for x, y in zip(s, b)))
    A("")
    A("== 4. What the sender can and cannot do today (no code was changed)")
    A("   EvnRmnMr: the planner gives every validator of a stratum the same quota, so EvnRmnMr would get the supplementary Firedancer quota too.")
    A("     `sender session` has NO --selection flag and its workers pass none (main.rs Cmd::Session, session::worker_args); `live` then takes the newest data/snapshots/selection_*.json (latest_selection, by file name).")
    A(f"     No code change is needed. The copy {SELECTION_COPY} (the original {SELECTION} without EvnRmnMr) is stored in the repo and copied byte for byte to {SELECTION_DEPLOYED} just before the session: it sorts after the original and is what session and workers load.")
    A("     It is NOT kept in data/snapshots between sessions, because tagged tools and tests (session_cmd.py, sessions.py, independence.py, test_session_cmd.py) read the newest selection_*.json there. `sender plan --selection <file>` takes a file explicitly.")
    A("   X gaps: the gap of an X window is x_gaps_ms[(X windows done) mod 5] (planner.rs line 285) with x_gaps_ms = [0, 10, 25, 60, 120] fixed in arms.rs. There is no switch: restricting to 0, 10, 25 ms needs a change in crates/ (not made).")
    A("   Without that change the supplementary X windows would again go to all five gaps in rotation (60 and 120 ms: 6 and 0 usable of 36 attempted each so far; 33 each without EvnRmnMr).")
    A("   Hours at supply assume the planner can use every leader window of a validator (no pacing or 5 s spacing skips), so they are a lower bound; the 44 an hour is the rate session 4 achieved.")
    A("   Pooled Agave: the same quota goes to 5Us18hLZ and 7QQGNm3p; the pooled cell target is met by the pair, 5Us18hLZ is the supply bottleneck.")
    A("")
    A("== 5. Summary")
    for cap in CAPS:
        q, c, lc, h, b = totals[cap]
        A(f"   cap {cap}: {q} supplementary attempted windows, {c:,.0f} lamports (+ {lc:,.0f} for the 4of4 leftover); {h:.1f} h at the binding rate; bottleneck {b}; {math.ceil(h / HOURS_PER_NIGHT)} night(s) of {HOURS_PER_NIGHT} h")
    return "\n".join(L) + "\n"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--data", default=str(ROOT / "data"))
    ap.add_argument("--alloc", default=str(ROOT / "analysis/sim/results/alloc_v2.json"))
    ap.add_argument("--session-file", default=str(ROOT / "analysis/sim/results/sessions/alloc_calibrated_session4of4.json"))
    ap.add_argument("--out-dir", default=str(ROOT / "analysis/sim/results/sessions"))
    ap.add_argument("--dryrun", default=str(ROOT / "docs/SUPPLEMENTARY_DRYRUN_FULL.txt"))
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--write", action="store_true")
    g.add_argument("--check", action="store_true")
    a = ap.parse_args(argv)
    data = pathlib.Path(a.data)
    alloc = json.loads(pathlib.Path(a.alloc).read_text(encoding="utf-8"))
    q4 = json.loads(pathlib.Path(a.session_file).read_text(encoding="utf-8"))
    counts = load_counts(data)
    tgt = targets(alloc)
    sup, epochs = supply(data)
    spend = q4["allocation"]["spend_per_window_lamports"]
    sha = sha256_file(pathlib.Path(__file__))
    out: dict[pathlib.Path, bytes] = {}
    for cap in CAPS:
        recs = allocation(counts, tgt, q4["full_plan_quota"], cap)
        out[pathlib.Path(a.out_dir) / f"alloc_supplementary_{cap}cap.json"] = dump(build_file(recs, q4, cap, counts))
    out[pathlib.Path(a.dryrun)] = dryrun_text(counts, tgt, q4, sup, epochs, spend, sha).encode("utf-8")
    bad = 0
    for p, b in out.items():
        if a.check:
            ok = p.exists() and p.read_bytes() == b
            print(("ok       " if ok else "DIFFERS  ") + str(p))
            bad += 0 if ok else 1
        else:
            p.write_bytes(b)
            print(f"wrote {p}  sha256 {hashlib.sha256(b).hexdigest()}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
