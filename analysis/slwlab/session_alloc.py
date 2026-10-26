"""Per-session allocation files with CUMULATIVE quotas (PREREGISTRATION §8).

``python -m slwlab.session_alloc --alloc sim/results/alloc_v2.json --scenario calibrated
--sessions 4 --out-dir sim/results/sessions`` writes ``alloc_<scenario>_session<k>of<n>.json``
for k = 1..n and prints their SHA-256. ``--check`` regenerates and compares with the files.

Why cumulative: the sender's planner (``runner::prior_plan_state``) reads the windows already
attempted for the purpose from the spend ledger joined with each run's ``plan.jsonl``, across
all earlier sessions, and plans only ``quota - done`` per validator and arm. A file for session k
holds ``ceil(quota * k / n)``. Session 1 gets a quarter, session 2 a half minus what session 1
did; a shortfall of session k therefore carries over to session k + 1 by itself, and the last
file holds the full quota. The planner has no per-session quota, so without this a validator
with many leader windows would fill its whole quota in the first hours.

The sender reads ``allocation.plan_quota`` (stratum -> arm -> attempted windows per validator,
the same for every validator of a stratum). BamFiredancer is dropped (PREREGISTRATION §8, §12).
The files are deterministic: sorted keys, LF line ends, no timestamps.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import sys

DROP = ("BamFiredancer",)


def cumulative(plan_quota: dict, k: int, n: int) -> dict:
    return {s: {a: (q * k + n - 1) // n for a, q in sorted(arms.items())} for s, arms in sorted(plan_quota.items())}


def build(alloc_doc: dict, scenario: str, sessions: int) -> list[dict]:
    block = alloc_doc["scenarios"][scenario]["allocation"] if scenario != "base" else alloc_doc["allocation"]
    full = {s: dict(a) for s, a in block["plan_quota"].items() if s not in DROP}
    files = []
    for k in range(1, sessions + 1):
        files.append({
            "label": "ALLOCATION FILE (derived from the SIMULATION result alloc_v2.json)",
            "source": {"file": "alloc_v2.json", "scenario": scenario, "seed": alloc_doc.get("seed"),
                       "reps": alloc_doc.get("reps")},
            "session": k, "sessions": sessions, "cumulative_share": f"{k}/{sessions}",
            "rounding": "plan_quota = ceil(full quota * k / sessions) per stratum and arm",
            "dropped_strata": list(DROP),
            "allocation": {"n": block["n"], "usable_rate": block["usable_rate"],
                           "spend_per_window_lamports": block["spend_per_window_lamports"],
                           "plan_quota": cumulative(full, k, sessions)},
            "full_plan_quota": cumulative(full, sessions, sessions),
        })
    return files


def dump(doc: dict) -> bytes:
    return (json.dumps(doc, indent=1, sort_keys=True) + "\n").encode("utf-8")


def sha256(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def name(scenario: str, k: int, n: int) -> str:
    return f"alloc_{scenario}_session{k}of{n}.json"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--alloc", default="sim/results/alloc_v2.json")
    ap.add_argument("--scenario", default="calibrated")
    ap.add_argument("--sessions", type=int, default=4)
    ap.add_argument("--out-dir", default="sim/results/sessions")
    ap.add_argument("--check", action="store_true", help="compare the files on disk with a fresh build")
    a = ap.parse_args(argv)
    doc = json.loads(pathlib.Path(a.alloc).read_text(encoding="utf-8"))
    out = pathlib.Path(a.out_dir)
    bad = 0
    for k, f in enumerate(build(doc, a.scenario, a.sessions), 1):
        data, p = dump(f), out / name(a.scenario, k, a.sessions)
        if a.check:
            ok = p.exists() and p.read_bytes() == data
            bad += not ok
        else:
            out.mkdir(parents=True, exist_ok=True)
            p.write_bytes(data)
            ok = True
        total = sum(sum(arms.values()) for arms in f["allocation"]["plan_quota"].values())
        print(f"{sha256(data)}  {p.as_posix()}  cumulative quotas {total} per-validator windows over {len(f['allocation']['plan_quota'])} strata"
              + ("" if ok else "  MISMATCH"))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
