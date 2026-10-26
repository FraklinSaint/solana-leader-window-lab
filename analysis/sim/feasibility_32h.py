"""PLANNING / SIMULATION: can the per-validator quotas of alloc_v2.json (equal inside a stratum, as the\nplanner reads them) be met in 4 sessions x 8 h? Replays the saved leader schedules (epochs 1065-1067)\nwith 15% / 30% lost windows, 5 s spacing and the spend pacing of planner.rs (session cap 38M/4 or\n1.25 x planned/4); the Agave quotas are scaled x1.0 / x1.25, or x10 to measure the supply.\nSession model of sessions.py: each session may take ceil(remaining / sessions left) of every quota.\nRun from analysis/:  python sim/feasibility_32h.py > sim/results/feasibility_32h.txt\n"""
import json
import math
import pathlib
import random
import statistics
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from slwlab import sessions as S

repo = pathlib.Path(__file__).resolve().parents[2]
data = repo / "data"
sel = json.loads((data / "snapshots" / "selection_20261022T163227Z.json").read_text(encoding="utf-8"))
alloc_doc = json.loads((repo / "analysis/sim/results/alloc_v2.json").read_text(encoding="utf-8"))
allocs = {"preset": alloc_doc["allocation"], "calibrated": alloc_doc["scenarios"]["calibrated"]["allocation"]}
b = S.ns_per_slot(data)
epochs = S.load_epochs(data)
t0 = epochs[0]["first_slot"]
span_h = sum(e["slots"] for e in epochs) * b / S.HOUR_NS
stream, counts = [], {}
for e in epochs:
    for f, _, l in S.windows(e):
        stream.append(((f - t0) * b, l))
        counts[l] = counts.get(l, 0) + 1
stream.sort()
picked = {p["identity"]: p["stratum"] for p in sel["picked"]}
ag = [i for i, s in picked.items() if s == "Agave"]
name = {i: i[:8] for i in ag}
L, N = 8, 4
blocks = [(k * L * S.HOUR_NS, (k + 1) * L * S.HOUR_NS) for k in range(N)]


def run(quota, loss, seed, spend, session_cap, per_session_cap=True):
    rng = random.Random(seed)
    done = {i: {a: 0 for a in q} for i, q in quota.items()}
    sessions_done = []
    for k, (b0, b1) in enumerate(blocks):
        left = len(blocks) - k
        cap = {i: {a: (math.ceil((q[a] - done[i][a]) / left) if per_session_cap else q[a] - done[i][a]) for a in q}
               for i, q in quota.items()}
        got = {i: {a: 0 for a in q} for i, q in quota.items()}
        last, spent = None, 0.0
        for t, ident in stream:
            if t < b0 or t >= b1 or ident not in quota:
                continue
            arm = S.pick_arm(cap[ident], got[ident])
            if arm is None:
                continue
            if last is not None and t - last < S.SPACING_NS:
                continue
            if rng.random() < loss:
                continue
            cost = spend[arm]
            if session_cap is not None and spent + cost > session_cap * (t - b0) / (b1 - b0) + cost:
                continue
            last = t
            spent += cost
            got[ident][arm] += 1
        for i in got:
            for a in got[i]:
                done[i][a] += got[i][a]
    return done


print(f"raw leader windows in 32 h: " + ", ".join(f"{name[i]} {counts[i]/span_h*32:.0f}" for i in ag))
for an, al in allocs.items():
    pq, spend = al["plan_quota"], al["spend_per_window_lamports"]
    base = {i: dict(pq[s]) for i, s in picked.items() if s in pq}
    target = sum(base[ag[0]].values())
    for loss in (0.15, 0.30):
        for capname, cap in (("pace 38M/4", 38_000_000 / 4), ("pace 1.25*plan/4", 1.25 * al["confirmatory_total_lamports"] / 4)):
            # supply: quotas x10 so only loss, spacing and pacing limit the windows
            big = {i: {a: (n * 10 if i in ag else n) for a, n in q.items()} for i, q in base.items()}
            sup = [run(big, loss, S.SEED + s, spend, cap) for s in range(10)]
            supply = {i: statistics.mean(sum(r[i].values()) for r in sup) for i in ag}
            row = [f"{an:10s} loss {loss:.0%} {capname:15s} quota/validator {target:3d}",
                   "supply(5Us,7QQ) " + ",".join(f"{supply[i]:.0f}" for i in ag[::-1])]
            for scale in (1.0, 1.25):
                q = {i: {a: (math.ceil(n * scale) if i in ag else n) for a, n in qq.items()} for i, qq in base.items()}
                res = [run(q, loss, S.SEED + s, spend, cap) for s in range(20)]
                ok = sum(all(r[i][a] >= q[i][a] for i in q for a in q[i]) for r in res)
                ok_ag = sum(all(r[i][a] >= q[i][a] for i in ag for a in q[i]) for r in res)
                att = {i: statistics.mean(sum(r[i].values()) for r in res) for i in ag}
                row.append(f"x{scale}: all-filled {ok}/20 (Agave pair {ok_ag}/20) attempted 5Us/7QQ {att[ag[1]]:.0f}/{att[ag[0]]:.0f} of {sum(q[ag[0]].values())}; min 5Us {min(sum(r[ag[1]].values()) for r in res)}")
            print(" | ".join(row), flush=True)
print("order of ag:", [name[i] for i in ag])
