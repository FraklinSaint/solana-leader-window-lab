#!/usr/bin/env python3
"""One-command reproduction of the confirmatory analysis from the committed data.

    python scripts/reproduce.py                 # verify the data offline, rerun the analysis, compare
    python scripts/reproduce.py --skip-verify   # rerun and compare only (minutes instead of about 20 minutes)
    docker compose run --rm reproduce           # the same inside the pinned image

Steps
1. scripts/verify_manifest.py (offline: manifest against the saved blocks, tx contents, exclusions, ledger).
2. The registered command, unchanged apart from the output path:
       python -m slwlab.analyze --manifest ../data/manifest.jsonl --sim sim/results/sim_v2.json \
           --data ../data --pooled Agave --out <OUT>/results.json          (run from analysis/)
3. scripts/positive_control_check.py on the new output (its result is printed; a failed control is a result,
   not a reproduction failure).
4. A number-by-number comparison of results.json and results_pooled_Agave.json with the committed ones in
   analysis/results/confirmatory_2026-11-03/. Keys, strings, booleans and integers must be identical; floats
   must agree to a relative 1e-9 (absolute 1e-12), so a different CPU or BLAS cannot cause a false alarm.

Exit code 0 only if every step ran and the comparison finds no difference. Nothing touches the network, and
nothing under data/ or analysis/results/ is written.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import pathlib
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
REFERENCE = ROOT / "analysis" / "results" / "confirmatory_2026-11-03"
FILES = ("results.json", "results_pooled_Agave.json")
REL, ABS = 1e-9, 1e-12


def compare(ref, new, path: str = "$", rel: float = REL, abs_: float = ABS, acc: dict | None = None) -> dict:
    """Walk two parsed JSON values. acc: {'numbers': n, 'max_rel': x, 'diffs': [(path, why)]}."""
    acc = acc if acc is not None else {"numbers": 0, "max_rel": 0.0, "diffs": []}

    def diff(why: str) -> None:
        acc["diffs"].append((path, why))

    if isinstance(ref, dict) and isinstance(new, dict):
        for k in sorted(set(ref) | set(new)):
            if k not in ref or k not in new:
                acc["diffs"].append((f"{path}.{k}", "key only in " + ("new" if k in new else "reference")))
            else:
                compare(ref[k], new[k], f"{path}.{k}", rel, abs_, acc)
    elif isinstance(ref, list) and isinstance(new, list):
        if len(ref) != len(new):
            diff(f"list length {len(ref)} vs {len(new)}")
        else:
            for i, (a, b) in enumerate(zip(ref, new)):
                compare(a, b, f"{path}[{i}]", rel, abs_, acc)
    elif isinstance(ref, bool) or isinstance(new, bool) or ref is None or new is None or isinstance(ref, str):
        if ref != new or type(ref) is not type(new):
            diff(f"{ref!r} vs {new!r}")
    elif isinstance(ref, (int, float)) and isinstance(new, (int, float)):
        acc["numbers"] += 1
        if isinstance(ref, int) and isinstance(new, int):
            if ref != new:
                diff(f"{ref} vs {new}")
        elif math.isnan(ref) or math.isnan(new):
            if not (math.isnan(ref) and math.isnan(new)):
                diff(f"{ref} vs {new}")
        elif math.isinf(ref) or math.isinf(new):
            if ref != new:
                diff(f"{ref} vs {new}")
        else:
            d = abs(ref - new)
            scale = max(abs(ref), abs(new))
            if scale > 0:
                acc["max_rel"] = max(acc["max_rel"], d / scale if d > abs_ else 0.0)
            if not math.isclose(ref, new, rel_tol=rel, abs_tol=abs_):
                diff(f"{ref!r} vs {new!r}")
    else:
        diff(f"types {type(ref).__name__} vs {type(new).__name__}")
    return acc


def run(cmd: list[str], cwd: pathlib.Path, env: dict) -> tuple[int, float, str]:
    t0 = time.monotonic()
    p = subprocess.run(cmd, cwd=cwd, env=env, text=True, encoding="utf-8", errors="replace",
                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    return p.returncode, time.monotonic() - t0, p.stdout


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--out", help="directory for the new results (default: a temporary directory)")
    ap.add_argument("--reference", default=str(REFERENCE), help="committed results to compare with")
    ap.add_argument("--skip-verify", action="store_true", help="skip the offline data verification (~20 min)")
    a = ap.parse_args(argv)

    out = pathlib.Path(a.out) if a.out else pathlib.Path(tempfile.mkdtemp(prefix="slwlab-repro-"))
    out.mkdir(parents=True, exist_ok=True)
    ref = pathlib.Path(a.reference)
    for f in FILES:
        if not (ref / f).is_file():
            print(f"missing reference file {ref / f}", file=sys.stderr)
            return 2
    env = dict(os.environ, PYTHONPATH=str(ROOT / "analysis"), PYTHONHASHSEED="0")
    py = sys.executable
    failed = False

    if a.skip_verify:
        print("1/4 verify_manifest: skipped (--skip-verify)")
    else:
        print("1/4 verify_manifest (offline, about 20 minutes) ...", flush=True)
        code, sec, text = run([py, str(ROOT / "scripts" / "verify_manifest.py"), "--out", str(out / "verify.json")],
                              ROOT, env)
        print(text.rstrip().splitlines()[-1] if text.strip() else "(no output)")
        print(f"    exit {code}, {sec:.0f} s")
        failed |= code != 0

    print("2/4 slwlab.analyze (the registered command) ...", flush=True)
    cmd = [py, "-m", "slwlab.analyze", "--manifest", "../data/manifest.jsonl", "--sim", "sim/results/sim_v2.json",
           "--data", "../data", "--pooled", "Agave", "--out", str(out / "results.json")]
    code, sec, text = run(cmd, ROOT / "analysis", env)
    print(text.rstrip())
    print(f"    exit {code}, {sec:.0f} s")
    if code != 0:
        print("NOT REPRODUCED: the analysis command failed", file=sys.stderr)
        return 1

    print("3/4 positive control on the new output ...", flush=True)
    code, sec, text = run([py, str(ROOT / "scripts" / "positive_control_check.py"), "--results", str(out / "results.json"),
                           "--pooled", str(out / "results_pooled_Agave.json"),
                           "--alloc", str(ROOT / "analysis" / "sim" / "results" / "alloc_v2.json")], ROOT, env)
    print(text.rstrip())
    if "POSITIVE CONTROL PASSED" not in text and "POSITIVE CONTROL *** FAILED" not in text:
        print("    the control script did not produce a verdict", file=sys.stderr)
        failed = True

    print("4/4 comparison with the committed results ...", flush=True)
    total, worst = 0, 0.0
    for f in FILES:
        acc = compare(json.loads((ref / f).read_text(encoding="utf-8")),
                      json.loads((out / f).read_text(encoding="utf-8")))
        total += acc["numbers"]
        worst = max(worst, acc["max_rel"])
        same = (ref / f).read_bytes() == (out / f).read_bytes()
        print(f"    {f}: {acc['numbers']} numbers, {len(acc['diffs'])} differences, "
              f"{'byte-identical' if same else 'not byte-identical'}")
        for p, why in acc["diffs"][:20]:
            print(f"      {p}: {why}")
        failed |= bool(acc["diffs"])

    if failed:
        print("NOT REPRODUCED (see the lines above)")
        return 1
    print(f"REPRODUCED: {total} numbers agree with the committed results, largest relative difference {worst:.2e}")
    print(f"new results are in {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
