"""Cross-process RPC rate limiter, Python twin of crates/sender/src/ratelimit.rs::FileBucket.

Same files, same format, same arithmetic, so Python tools (verify_facts.py) and every
`sender` process on the machine share ONE budget:

    <dir>/<name>.lock   exclusive OS lock (msvcrt on Windows, flock elsewhere)
    <dir>/<name>.gcra   {"tat_ns": <int>}  theoretical arrival time (GCRA)

dir = $SLWLAB_RATELIMIT_DIR or <repo>/data/.ratelimit. Stdlib only.
"""

from __future__ import annotations

import json
import os
import pathlib
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
MAX_RPC_PER_SEC = 8.0
RPC_BURST = 2.0
MAX_AHEAD_NS = 30_000_000_000


def default_dir() -> pathlib.Path:
    d = os.environ.get("SLWLAB_RATELIMIT_DIR")
    return pathlib.Path(d) if d else ROOT / "data" / ".ratelimit"


if sys.platform == "win32":
    import msvcrt

    def _lock(f) -> None:
        # Byte 0 overlaps the whole-file range Rust's File::lock takes (LockFileEx).
        f.seek(0)
        while True:
            try:
                msvcrt.locking(f.fileno(), msvcrt.LK_LOCK, 1)
                return
            except OSError:  # LK_LOCK gives up after ~10 s; keep waiting
                continue

    def _unlock(f) -> None:
        f.seek(0)
        msvcrt.locking(f.fileno(), msvcrt.LK_UNLCK, 1)

else:
    import fcntl

    def _lock(f) -> None:
        fcntl.flock(f.fileno(), fcntl.LOCK_EX)

    def _unlock(f) -> None:
        fcntl.flock(f.fileno(), fcntl.LOCK_UN)


class FileBucket:
    def __init__(self, directory: pathlib.Path | None = None, name: str = "rpc",
                 rate: float = MAX_RPC_PER_SEC, burst: float = RPC_BURST):
        assert 0 < rate <= MAX_RPC_PER_SEC and burst >= 1
        self.dir = pathlib.Path(directory) if directory else default_dir()
        self.name = name
        self.interval_ns = int(-(-1e9 // rate))  # ceil, as in Rust
        self.tau_ns = int((burst - 1) * self.interval_ns)

    def reserve(self) -> int:
        """Reserve the next slot; return its UNIX time in ns (now or later)."""
        self.dir.mkdir(parents=True, exist_ok=True)
        with open(self.dir / f"{self.name}.lock", "a+b") as lf:
            _lock(lf)
            try:
                state = self.dir / f"{self.name}.gcra"
                try:
                    stored = int(json.loads(state.read_text())["tat_ns"])
                except (OSError, ValueError, KeyError, TypeError):
                    stored = 0
                now = time.time_ns()
                tat = max(stored, now)
                if tat - now > MAX_AHEAD_NS:
                    tat = now  # wall clock jumped backwards
                at = max(now, tat - self.tau_ns)
                state.write_text(f'{{"tat_ns": {tat + self.interval_ns}}}\n')
            finally:
                _unlock(lf)
        return at

    def acquire(self) -> int:
        at = self.reserve()
        wait = at - time.time_ns()
        if wait > 0:
            time.sleep(wait / 1e9)
        return at
