"""Copy data/manifest.jsonl without the lines of one session (pre-specified exploratory sensitivity
analysis "no round 2"; NOT part of the confirmatory verdict).

    python scripts/filter_manifest_session.py --manifest data/manifest.jsonl --data data \
        --session 20261102T123836Z --out analysis/results/<dir>/manifest_no_round2.jsonl

The run ids of the session are read from data/sessions/<id>/coverage.json ("runs"), and every one
must occur in the manifest. A line is dropped if and only if its tx.run_id is one of them. All other
lines are copied byte for byte (same order, same line endings). It reads no outcome, rule or order
information. It refuses to overwrite an existing output and refuses an output path inside data/.
Prints line counts and SHA-256 of input and output.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib


def run_ids_of_session(data: pathlib.Path, session: str) -> set[str]:
    cov = json.loads((data / "sessions" / session / "coverage.json").read_text(encoding="utf-8"))
    ids = {str(r).replace("\\", "/").rstrip("/").split("/")[-1] for r in cov["runs"]}
    if not ids:
        raise SystemExit(f"session {session}: coverage.json lists no runs")
    return ids


def filter_lines(raw_lines: list[bytes], drop_runs: set[str]) -> tuple[list[bytes], dict[str, int]]:
    kept, seen = [], {r: 0 for r in drop_runs}
    for raw in raw_lines:
        if not raw.strip():
            kept.append(raw)
            continue
        run = json.loads(raw)["tx"]["run_id"]
        if run in drop_runs:
            seen[run] += 1
        else:
            kept.append(raw)
    return kept, seen


def sha(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--data", required=True)
    ap.add_argument("--session", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    out = pathlib.Path(a.out).resolve()
    data = pathlib.Path(a.data).resolve()
    if data in out.parents:
        raise SystemExit("output must not be inside the data directory")
    if out.exists():
        raise SystemExit(f"{out} exists: write-once, choose a new name")
    src = pathlib.Path(a.manifest).read_bytes()
    drop = run_ids_of_session(data, a.session)
    kept, seen = filter_lines(src.splitlines(keepends=True), drop)
    missing = sorted(r for r, n in seen.items() if n == 0)
    if missing:
        raise SystemExit(f"run id(s) of session {a.session} not in the manifest: {missing}")
    out.parent.mkdir(parents=True, exist_ok=True)
    body = b"".join(kept)
    out.write_bytes(body)
    print(f"session {a.session}: dropped {sum(seen.values())} lines from run(s) {sorted(seen)}")
    print(f"input  {len(src.splitlines())} lines sha256 {sha(src)}")
    print(f"output {len(kept)} lines sha256 {sha(body)} -> {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
