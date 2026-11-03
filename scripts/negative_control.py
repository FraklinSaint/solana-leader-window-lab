"""Negative control (PREREGISTRATION section 7; fixed in docs/ANALYSIS_CLARIFICATIONS_PRE_RUN.md 1b).

    python scripts/negative_control.py --manifest data/manifest.jsonl --sim analysis/sim/results/sim_v2.json \
        --data data --out analysis/results/<dir>/negative_control.json [--shuffles 20] [--pooled Agave]

For r = 0 .. R-1 (R = 20), inside every USABLE window the realized landing order of its txs is permuted
uniformly at random (the txs of the window swap their block positions; the set of positions, the arrival
times of the txs, the rules, the arms and everything else stay). The existing functions
`slwlab.analyze.analyze` and `analyze.analyze_pooled` are then run on the shuffled windows with the same
arguments, seeds and Holm families as the real run (`analyze.main`). Nothing in analysis/slwlab is changed:
`parse.windows_from_manifest` is wrapped for the duration of each shuffled run and restored afterwards.

Randomness: shuffle r uses seed 20261022 + r. Each window gets its own generator seeded with
SeedSequence([20261022 + r, crc32(window_id)]), so the permutation of a window does not depend on the order
in which windows are visited, and `analyze` and `analyze_pooled` see the same shuffled data. The analysis
functions keep their own default seed (20261022), as in the real run.

Holm families: (validator, arm) for the per-validator table of `analyze`, and (pooled stratum, arm) for the
pooled table. A family has a hit in a shuffle if at least one of its rules has a finite Holm-adjusted p below
0.05 (the one-sided alpha of section 7). Pass iff every family has at most 3 hits in the 20 shuffles.
If shuffle 0 takes longer than 180 s, the pooled cells are dropped for ALL shuffles and the output says so.
NEVER run on the real data until the operator says so; unit tests use synthetic data only.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import math
import pathlib
import sys
import time
import zlib

import numpy as np

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "analysis"))

from slwlab import analyze, blocklist, parse  # noqa: E402

BASE_SEED = 20261022
SHUFFLES = 20
ALPHA = 0.05
MAX_HITS = 3
POOLED_TIME_LIMIT_S = 180.0


def shuffle_windows(windows: list, seed: int) -> None:
    """Permute, inside every usable window, which tx took which block position (in place)."""
    for w in windows:
        if not w.usable:
            continue
        rng = np.random.default_rng(np.random.SeedSequence([seed, zlib.crc32(w.window_id.encode("utf-8"))]))
        perm = rng.permutation(len(w.landed))
        w.landed = [w.landed[i] for i in perm]
        w.send_ns = [w.send_ns[i] for i in perm]
        # w.positions (the block indices, ascending) stays: the txs are re-assigned to them


@contextlib.contextmanager
def shuffled_windows(seed: int):
    original = parse.windows_from_manifest

    def wrapped(*args, **kwargs):
        ws = original(*args, **kwargs)
        shuffle_windows(ws, seed)
        return ws

    parse.windows_from_manifest = wrapped
    try:
        yield
    finally:
        parse.windows_from_manifest = original


def has_hit(rules: dict) -> bool:
    for v in rules.values():
        p = v.get("p_holm")
        if p is not None and not math.isnan(p) and p < ALPHA:
            return True
    return False


def families_of(res: dict, pooled: dict | None, pooled_stratum: str) -> dict[str, bool]:
    out = {}
    for c in res["cells"]:
        out[f"validator {c['identity']} arm {c['arm']}"] = has_hit(c["rules"])
    if pooled is not None:
        for c in pooled["cells"]:
            out[f"pooled {pooled_stratum} arm {c['arm']}"] = has_hit(c["rules"])
    return out


def run_shuffles(lines: list[dict], sim: dict, present, require_slot_status: bool, n: int = SHUFFLES,
                 base_seed: int = BASE_SEED, pooled_stratum: str | None = "Agave", ref_reps: int | None = None,
                 time_limit_s: float = POOLED_TIME_LIMIT_S, clock=time.monotonic) -> dict:
    hits: dict[str, int] = {}
    pooled_dropped = False
    seconds = []
    for r in range(n):
        t0 = clock()
        with shuffled_windows(base_seed + r):
            res = analyze.analyze(lines, sim, present=present, include_skipped=False,
                                  require_slot_status=require_slot_status)
            pooled = None
            if pooled_stratum and not pooled_dropped:
                kw = {} if ref_reps is None else {"ref_reps": ref_reps}
                pooled = analyze.analyze_pooled(lines, sim, pooled_stratum, present=present, include_skipped=False,
                                                require_slot_status=False, **kw)
        dt_s = clock() - t0
        seconds.append(dt_s)
        if r == 0 and pooled_stratum and dt_s > time_limit_s:
            pooled_dropped = True  # drop the pooled cells for every shuffle, including this one
            pooled = None
        for fam, hit in families_of(res, pooled, pooled_stratum or "").items():
            hits[fam] = hits.get(fam, 0) + int(hit)
    over = {f: k for f, k in hits.items() if k > MAX_HITS}
    return {
        "label": "NEGATIVE CONTROL (PREREGISTRATION section 7)",
        "shuffles": n, "base_seed": base_seed, "alpha_holm_one_sided": ALPHA, "max_hits_per_family": MAX_HITS,
        "pooled_stratum": pooled_stratum, "pooled_dropped_for_time": pooled_dropped,
        "pooled_time_limit_s": time_limit_s, "seconds_per_shuffle": seconds,
        "families": len(hits), "hits_per_family": dict(sorted(hits.items())),
        "families_over_limit": over, "pass": not over,
    }


def load_inputs(manifest: str, sim_path: str, data: str | None):
    """Same loading and refusals as analyze.main."""
    lines = parse.read_jsonl(pathlib.Path(manifest))
    sim = json.loads(pathlib.Path(sim_path).read_text(encoding="utf-8"))
    needed = blocklist.runs_with_purpose(lines, "measure")
    if needed and not data:
        raise SystemExit("this manifest needs the stored getBlocks lists (PREREGISTRATION section 9): pass --data <data dir>")
    present = None
    if data:
        d = pathlib.Path(data)
        missing = [r for r in needed if blocklist.load(d, r) is None]
        if missing:
            raise SystemExit("no stored getBlocks list for run(s) " + ", ".join(missing))
        present = blocklist.present_map(d, sorted({l["tx"]["run_id"] for l in lines}))
    return lines, sim, present, bool(needed)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--sim", required=True)
    ap.add_argument("--data")
    ap.add_argument("--out", required=True)
    ap.add_argument("--shuffles", type=int, default=SHUFFLES)
    ap.add_argument("--pooled", default="Agave", metavar="STRATUM", help="pooled stratum, or 'none'")
    a = ap.parse_args(argv)
    out = pathlib.Path(a.out)
    if out.exists():
        raise SystemExit(f"{out} exists: write-once, choose a new name")
    lines, sim, present, req = load_inputs(a.manifest, a.sim, a.data)
    res = run_shuffles(lines, sim, present, req, n=a.shuffles, pooled_stratum=None if a.pooled == "none" else a.pooled)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(res, indent=1, sort_keys=True) + "\n", encoding="utf-8", newline="\n")
    print(f"{res['families']} families, {res['shuffles']} shuffles, pooled dropped for time: {res['pooled_dropped_for_time']}")
    for f, k in res["families_over_limit"].items():
        print(f"OVER LIMIT: {f}: {k} of {res['shuffles']} shuffles with a significant rule (limit {MAX_HITS})")
    print("NEGATIVE CONTROL " + ("PASSED" if res["pass"] else "FAILED") + f" -> {out}")
    return 0 if res["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
