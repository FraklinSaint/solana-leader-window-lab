"""Stored getBlocks lists: which slots of every window have a block (PREREGISTRATION §9).

A window with a skipped slot is excluded as `skipped_slot`. The decision must come from the
chain's own record of which slots have blocks, never from our transactions. The sender binary
does not fetch this. After a run (once its last window is finalized), run

    RPC_URL=<mainnet rpc> python -m slwlab.blocklist fetch --data ../data --manifest <manifest>

It reads the manifest, asks `getBlocks` (finalized) for the slot ranges of its windows (merged
into a few calls), and saves `data/logs/<run_id>/blocklist.json` next to the run's plan and
windows files. Read-only RPC; the key is never printed or stored. `parse.windows_from_manifest`
classifies from these files, and `scripts/verify_manifest.py` check K verifies them.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import re
import time
import urllib.error
import urllib.request
from collections.abc import Callable

FILE = "blocklist.json"
COMMITMENT = "finalized"
MAX_GAP = 2_000        # windows closer than this many slots share one getBlocks call
MAX_SPAN = 100_000     # slots per call (the RPC limit is 500,000)


def redact(s: str) -> str:
    return re.sub(r"(?i)((?:x-)?api[-_]?key=)[^&\s\"'<>#),;\\]+", r"\1REDACTED", s)


def windows_by_run(lines: list[dict]) -> dict[str, dict[str, tuple[int, int]]]:
    """run_id -> window_id -> (first_slot, n_slots), for ordering-eligible windows."""
    out: dict[str, dict[str, tuple[int, int]]] = {}
    for l in lines:
        t = l["tx"]
        if not t.get("ordering_eligible", True):
            continue
        out.setdefault(t["run_id"], {})[t["window_id"]] = (t["first_slot"], t["n_slots"])
    return out


def merge_spans(spans: list[tuple[int, int]], max_gap: int = MAX_GAP, max_span: int = MAX_SPAN) -> list[tuple[int, int]]:
    """Inclusive (start, end) slot ranges, merged when close; no merged range is longer than max_span."""
    out: list[list[int]] = []
    for a, b in sorted(spans):
        if out and a - out[-1][1] <= max_gap and b - out[-1][0] < max_span:
            out[-1][1] = max(out[-1][1], b)
        else:
            out.append([a, b])
    return [(a, b) for a, b in out]


def build_record(run_id: str, windows: dict[str, tuple[int, int]], rpc: Callable[[str, list], object],
                 now: dt.datetime | None = None) -> dict:
    """One run's record. `rpc(method, params)` returns the JSON-RPC result."""
    fin = int(rpc("getSlot", [{"commitment": COMMITMENT}]))
    last = max(f + n - 1 for f, n in windows.values())
    if last > fin:
        raise RuntimeError(f"run {run_id}: window slots up to {last} are not finalized yet (finalized {fin})")
    calls, present = [], set()
    for a, b in merge_spans([(f, f + n - 1) for f, n in windows.values()]):
        got = rpc("getBlocks", [a, b, {"commitment": COMMITMENT}])
        got = sorted({int(s) for s in got})
        if any(s < a or s > b for s in got):
            raise RuntimeError(f"getBlocks({a},{b}) returned slots outside the range")
        present.update(got)
        calls.append({"start": a, "end": b, "n_blocks": len(got)})
    now = now or dt.datetime.now(dt.timezone.utc)
    return {
        "label": "SLOT LIST (getBlocks)", "method": "getBlocks", "commitment": COMMITMENT,
        "run_id": run_id, "fetched_utc": now.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "finalized_slot_at_fetch": fin, "calls": calls,
        "windows": {wid: {"first_slot": f, "n_slots": n,
                          "slots_with_blocks": [s for s in range(f, f + n) if s in present]}
                    for wid, (f, n) in sorted(windows.items(), key=lambda kv: kv[1])},
    }


def path_for(data_root: pathlib.Path, run_id: str) -> pathlib.Path:
    return data_root / "logs" / run_id / FILE


def load(data_root: pathlib.Path, run_id: str) -> dict | None:
    p = path_for(data_root, run_id)
    return json.loads(p.read_text(encoding="utf-8")) if p.exists() else None


def present_map(data_root: pathlib.Path, run_ids) -> dict[str, set[int]]:
    """window_id -> slots with a block, from the stored lists of `run_ids` (missing lists: no entry)."""
    out: dict[str, set[int]] = {}
    for rid in run_ids:
        rec = load(data_root, rid)
        for wid, w in (rec or {}).get("windows", {}).items():
            out[wid] = set(w["slots_with_blocks"])
    return out


def save(data_root: pathlib.Path, rec: dict) -> pathlib.Path:
    p = path_for(data_root, rec["run_id"])
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".tmp")
    tmp.write_text(json.dumps(rec, indent=1) + "\n", encoding="utf-8", newline="\n")
    tmp.replace(p)
    return p


class HttpRpc:
    """Minimal JSON-RPC client. At most one call per `min_interval` seconds (the free plan allows
    8 requests a second); the URL (which may hold a key) is never printed."""

    def __init__(self, url: str, min_interval: float = 0.25) -> None:
        self.url, self.min_interval, self.last = url, min_interval, 0.0
        self.calls = 0

    def __call__(self, method: str, params: list):
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
        for attempt in range(6):
            time.sleep(max(0.0, self.last + self.min_interval - time.monotonic()))
            self.last = time.monotonic()
            self.calls += 1
            req = urllib.request.Request(self.url, body, {"content-type": "application/json"})
            try:
                with urllib.request.urlopen(req, timeout=120) as r:
                    resp = json.load(r)
            except urllib.error.HTTPError as e:
                if e.code == 429:
                    time.sleep(min(0.25 * 2 ** attempt, 8))
                    continue
                raise RuntimeError(f"{method}: HTTP {e.code}") from None
            except urllib.error.URLError as e:
                raise RuntimeError(f"{method}: {redact(str(e.reason))}") from None
            if resp.get("error"):
                raise RuntimeError(f"{method}: {redact(str(resp['error']))[:200]}")
            return resp["result"]
        raise RuntimeError(f"{method}: rate-limited after 6 retries")


def fetch(lines: list[dict], data_root: pathlib.Path, rpc: Callable[[str, list], object],
          overwrite: bool = False, log=print) -> list[pathlib.Path]:
    written = []
    for run_id, windows in sorted(windows_by_run(lines).items()):
        if not overwrite and path_for(data_root, run_id).exists():
            log(f"{run_id}: list exists, skipped")
            continue
        rec = build_record(run_id, windows, rpc)
        written.append(save(data_root, rec))
        log(f"{run_id}: {len(windows)} windows, {len(rec['calls'])} getBlocks call(s)")
    return written


def main(argv: list[str] | None = None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("fetch", help="getBlocks for every window of the manifest (RPC_URL from the environment)")
    f.add_argument("--data", default="../data")
    f.add_argument("--manifest", action="append", required=True)
    f.add_argument("--overwrite", action="store_true")
    a = ap.parse_args(argv)
    url = os.environ.get("RPC_URL", "")
    if not url:
        raise SystemExit("fetch needs RPC_URL in the environment")
    lines = [json.loads(l) for m in a.manifest for l in pathlib.Path(m).read_text(encoding="utf-8").splitlines() if l.strip()]
    rpc = HttpRpc(url)
    try:
        fetch(lines, pathlib.Path(a.data), rpc)
    except RuntimeError as e:
        raise SystemExit(redact(str(e))) from None
    print(f"{rpc.calls} RPC calls")


if __name__ == "__main__":
    main()
