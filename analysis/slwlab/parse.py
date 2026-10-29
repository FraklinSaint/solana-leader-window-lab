"""Manifest + saved blocks -> analysis windows (PREREGISTRATION §9).

Input: `data/manifest.jsonl` (confirmatory) or `data/exploratory/calibration_manifest.jsonl`,
one line per sent tx, as written by the collector (crates/sender/src/collect.rs). Positions
come from the manifest's `block_index`. `verify_positions` re-reads each saved block
(`data/raw/blocks/<slot>.json.gz`) and checks them independently.
"""

from __future__ import annotations

import datetime as dt
import gzip
import json
import math
import pathlib
from collections import Counter, defaultdict
from dataclasses import dataclass, field

from . import ALPENGLOW_GENESIS_SLOT
from .model import Tx


def usable_min(n: int) -> int:
    """>= 6 of 8 landed in one slot (§9), scaled with N."""
    return math.ceil(0.75 * n)


@dataclass
class Window:
    window_id: str
    run_id: str
    identity: str
    vote_account: str
    stratum: str
    arm: str
    gap_us: int
    offset_ms: int
    first_slot: int
    n_slots: int
    purpose: str
    txs: list[Tx] = field(default_factory=list)  # all sent, by idx
    lines: list[dict] = field(default_factory=list)
    # filled by classify()
    slot: int | None = None
    landed: list[Tx] = field(default_factory=list)  # usable txs, in block order
    positions: list[int] = field(default_factory=list)  # block_index of `landed`
    send_ns: list[int] = field(default_factory=list)  # actual send start, for `landed`
    usable: bool = False
    reason: str | None = None
    paths: Counter = field(default_factory=Counter)
    # §9 skipped slots, from the stored getBlocks list (slwlab.blocklist), never from our txs
    slot_status: str = "unknown"  # "unknown" (no list given), "complete" or "skipped"
    skipped: list[int] = field(default_factory=list)
    # earliest actual send start (unix ns) of the window's txs that left the host; None if unknown
    send_unix_ns: int | None = None

    @property
    def n(self) -> int:
        return len(self.txs)

    @property
    def run_date(self) -> str:
        """The day the RUN started (the old `date`): a run that crosses 00:00Z keeps one day."""
        return self.run_id.split("-")[-1][:8] if "-" in self.run_id else ""

    @property
    def date(self) -> str:
        """UTC day (YYYYMMDD) of the window's send time. Falls back to `run_date` when the
        manifest line carries no send time (synthetic data)."""
        if self.send_unix_ns is None:
            return self.run_date
        return dt.datetime.fromtimestamp(self.send_unix_ns / 1e9, tz=dt.timezone.utc).strftime("%Y%m%d")


def read_jsonl(path: pathlib.Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(l) for l in path.read_text(encoding="utf-8").splitlines() if l.strip()]


def windows_from_manifest(lines: list[dict], present: dict[str, set[int]] | None = None,
                          include_skipped: bool = False, require_slot_status: bool = False) -> list[Window]:
    """`present`: window_id -> slots that have a block (getBlocks, `slwlab.blocklist.present_map`).
    With it, a window with any of its `n_slots` slots missing is excluded as `skipped_slot`
    (PREREGISTRATION §9); `include_skipped=True` is the sensitivity analysis that keeps them.
    `require_slot_status`: a window with no stored list is excluded as `slot_status_missing`
    (confirmatory data must never be classified without the list). Without `present` nothing
    changes, so exploratory code paths behave as before."""
    by: dict[str, Window] = {}
    for line in lines:
        t, o = line["tx"], line["outcome"]
        w = by.get(t["window_id"])
        if w is None:
            w = by[t["window_id"]] = Window(
                window_id=t["window_id"], run_id=t["run_id"], identity=t["identity"],
                vote_account=t["vote_account"], stratum=t["stratum"], arm=t["arm"],
                gap_us=t["gap_us"], offset_ms=t["offset_ms"], first_slot=t["first_slot"],
                n_slots=t["n_slots"], purpose=t["purpose"],
            )
        w.lines.append(line)
        w.txs.append(Tx(idx=t["idx"], cu_limit=t["cu_limit"], price=t["cu_price_micro_lamports"],
                        send_rank=t["send_rank"], send_offset_us=t["send"]["planned_offset_us"],
                        conflict=t.get("conflict_account") is not None,
                        signature=t["sig"], payer=t["payer"]))
        w.paths[t["send"]["path"]] += 1
        ns = t["send"].get("start_unix_ns")
        if ns is not None and t["send"].get("error") is None and (w.send_unix_ns is None or ns < w.send_unix_ns):
            w.send_unix_ns = int(ns)
        _ = o
    for w in by.values():
        order = sorted(range(len(w.txs)), key=lambda i: w.txs[i].idx)
        w.txs = [w.txs[i] for i in order]
        w.lines = [w.lines[i] for i in order]
        got = present.get(w.window_id) if present is not None else None
        if got is not None:
            w.skipped = [s for s in range(w.first_slot, w.first_slot + w.n_slots) if s not in got]
            w.slot_status = "skipped" if w.skipped else "complete"
        classify(w, include_skipped, require_slot_status)
    return sorted(by.values(), key=lambda w: (w.first_slot, w.window_id))


def classify(w: Window, include_skipped: bool = False, require_slot_status: bool = False) -> None:
    """Usable = ordering-eligible, >= usable_min(N) txs landed in ONE slot of the intended
    window, produced by the intended leader, with no collector exclusion, and (when the
    getBlocks list is known) none of the window's slots skipped."""
    if any(not l["tx"].get("ordering_eligible", True) for l in w.lines):
        w.reason = "delivery_check_path"
        return
    if any(l["tx"]["first_slot"] < ALPENGLOW_GENESIS_SLOT for l in w.lines):
        w.reason = "pre_alpenglow"
        return
    if w.slot_status == "unknown" and require_slot_status:
        w.reason = "slot_status_missing"
        return
    if w.skipped and not include_skipped:
        w.reason = "skipped_slot"
        return
    # PREREGISTRATION §6c: the actual send order must be the planned send_rank order.
    sent = sorted((l for l in w.lines if l["tx"]["send"].get("error") is None),
                  key=lambda l: int(l["tx"]["send"]["start_mono_ns"]))
    ranks = [l["tx"]["send_rank"] for l in sent]
    if ranks != sorted(ranks):
        w.reason = "send_order_mismatch"
        return
    ok = [l for l in w.lines if l["outcome"]["status"] == "landed" and not l["outcome"].get("exclusion_reason")]
    if not ok:
        w.reason = "none_landed_clean"
        return
    slot, cnt = Counter(l["outcome"]["slot"] for l in ok).most_common(1)[0]
    if cnt < usable_min(w.n):
        w.reason = f"fewer_than_{usable_min(w.n)}_in_one_slot"
        return
    same = sorted((l for l in ok if l["outcome"]["slot"] == slot), key=lambda l: l["outcome"]["block_index"])
    by_sig = {t.signature: t for t in w.txs}
    w.slot = slot
    w.landed = [by_sig[l["tx"]["sig"]] for l in same]
    w.positions = [l["outcome"]["block_index"] for l in same]
    w.send_ns = [int(l["tx"]["send"]["start_mono_ns"]) for l in same]
    w.usable = True


def exclusion_counts(lines: list[dict]) -> Counter:
    return Counter(l["outcome"].get("exclusion_reason") or l["outcome"]["status"] for l in lines)


def load_block(data_root: pathlib.Path, slot: int) -> dict:
    with gzip.open(data_root / "raw" / "blocks" / f"{slot}.json.gz", "rt", encoding="utf-8") as f:
        return json.load(f)


def block_producer(block: dict) -> str | None:
    fees = [r["pubkey"] for r in block.get("rewards") or [] if r.get("rewardType") == "Fee"]
    return fees[0] if len(fees) == 1 else None


def verify_positions(w: Window, data_root: pathlib.Path) -> list[str]:
    """Independent re-check of a usable window against its saved block."""
    problems = []
    block = load_block(data_root, w.slot)
    sigs = [t["transaction"]["signatures"][0] for t in block["transactions"]]
    for t, p in zip(w.landed, w.positions):
        if p >= len(sigs) or sigs[p] != t.signature:
            problems.append(f"{w.window_id}: {t.signature[:12]} not at index {p}")
    prod = block_producer(block)
    if prod != w.identity:
        problems.append(f"{w.window_id}: block {w.slot} produced by {prod}, expected {w.identity}")
    return problems


def supplementary(w: Window, data_root: pathlib.Path) -> dict:
    """SUPPLEMENTARY (other users' txs in the same block): block size and where ours sit."""
    block = load_block(data_root, w.slot)
    n = len(block["transactions"])
    return {"label": "SUPPLEMENTARY", "block_txs": n,
            "our_first_index": min(w.positions), "our_last_index": max(w.positions),
            "our_span": max(w.positions) - min(w.positions) + 1}


def skipped_report(windows: list[Window]) -> dict[str, dict]:
    """§9: per validator, the windows with a skipped slot, the windows total and the share.
    Only ordering-eligible windows whose slot list is known count."""
    out: dict[str, dict] = {}
    for w in windows:
        if w.slot_status == "unknown" or w.reason in ("delivery_check_path", "pre_alpenglow"):
            continue
        r = out.setdefault(w.identity, {"windows": 0, "windows_with_skipped_slot": 0, "skipped_slots": 0})
        r["windows"] += 1
        r["windows_with_skipped_slot"] += bool(w.skipped)
        r["skipped_slots"] += len(w.skipped)
    for r in out.values():
        r["share"] = r["windows_with_skipped_slot"] / r["windows"] if r["windows"] else None
    return dict(sorted(out.items()))


def by_validator_arm(windows: list[Window]) -> dict[tuple[str, str], list[Window]]:
    g: dict[tuple[str, str], list[Window]] = defaultdict(list)
    for w in windows:
        g[(w.identity, w.arm)].append(w)
    return g
