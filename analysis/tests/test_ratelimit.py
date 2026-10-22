"""scripts/ratelimit.py: the cross-process bucket bounds all processes together and uses the
same state format as the Rust FileBucket."""

import importlib.util
import json
import multiprocessing as mp
import pathlib
import re
import time

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("ratelimit", ROOT / "scripts" / "ratelimit.py")
rl = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rl)


def _child(directory: str, n: int, q) -> None:
    b = rl.FileBucket(pathlib.Path(directory), "rpc", rate=8.0, burst=2.0)
    q.put([b.reserve() for _ in range(n)])


def _max_in_window(ts: list[int], w_ns: int) -> int:
    best, j = 0, 0
    for i in range(len(ts)):
        while ts[i] - ts[j] >= w_ns:
            j += 1
        best = max(best, i - j + 1)
    return best


def test_three_processes_share_one_budget(tmp_path):
    q = mp.Queue()
    procs = [mp.Process(target=_child, args=(str(tmp_path), 8, q)) for _ in range(3)]
    for p in procs:
        p.start()
    ts = sorted(t for _ in procs for t in q.get(timeout=60))
    for p in procs:
        p.join()
    assert len(ts) == 24
    # 8/s with burst 2: at most 10 starts in any 1 s, and 24 slots need >= 22 intervals.
    assert _max_in_window(ts, 1_000_000_000) <= 10
    assert ts[-1] - ts[0] >= 22 * 125_000_000


def test_state_format_matches_rust():
    rust = (ROOT / "crates" / "sender" / "src" / "ratelimit.rs").read_text(encoding="utf-8")
    assert 'pub const SHARED_RPC: &str = "rpc";' in rust
    assert 'pub const SHARED_SEND_TX: &str = "send_tx";' in rust
    assert re.search(r'\\"tat_ns\\": \{new_tat\}', rust), "Rust writes {\"tat_ns\": N}"
    assert ".gcra" in rust and ".lock" in rust


def test_reads_state_written_like_rust(tmp_path):
    far = time.time_ns() + 3_600 * 10**9  # backwards clock jump must not stall
    (tmp_path / "rpc.gcra").write_text(json.dumps({"tat_ns": far}))
    b = rl.FileBucket(tmp_path, "rpc")
    assert b.reserve() - time.time_ns() < 10**9
    assert "tat_ns" in json.loads((tmp_path / "rpc.gcra").read_text())
