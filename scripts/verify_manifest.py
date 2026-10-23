#!/usr/bin/env python3
"""Independent check of every measurement claim in the committed data (README "Verify").

Read-only: never signs or sends a transaction. Stdlib only, so it runs anywhere.

    python scripts/verify_manifest.py                      # offline: committed files only
    RPC_URL=<any mainnet RPC> python scripts/verify_manifest.py --online

Offline, from `data/` alone (no trust in the collector):
  M  manifest: unique signatures, cluster = mainnet, every slot >= the Alpenglow guard.
  B  every landed tx sits at its claimed `block_index` of the saved block, with the claimed
     tx count, fee, error and producer (the block's single Fee reward, VERIFIED_FACTS F8).
  T  the on-chain tx says what the manifest says: fee payer, recent blockhash, signature
     count, and the ComputeBudget instructions (CU limit, CU price, loaded-data limit) that
     the ordering rules use. Arm C txs write-lock their shared account; every other tx
     write-locks only its own payer (so our txs never conflict outside Arm C).
  P  every window was planned before sending: `logs/<run>/plan.jsonl` holds it with the same
     leader (from getSlotLeaders at plan time), slots, arm, and per-tx CU limit, price and
     send rank.
  E  the leader-schedule snapshot of each window's epoch
     (`snapshots/leader_schedule_epoch<E>.json.gz`, raw getLeaderSchedule saved when the epoch
     was first planned) names the window's validator for all its slots and for the landed slot.
     Providers keep schedules for only ~10 epochs, so this is the durable leader evidence.
  X  `exclusion_reason` is re-derived from the block (producer, window bounds), not trusted.
  H  hidden data: no manifest tx marked "not landed" appears in any saved block, and no tx
     paid by one of our payers appears in a saved block without a manifest line.
  O  send order: in every window, the order of the actual send start times (`start_mono_ns`)
     equals the planned `send_rank` order. (Until 2026-10-23 the burst loop sent zero-gap arms
     in idx order; LOGBOOK 13:40.) Also for dry-run output: `--windows <run dir>`.
  Q  platform: every calibration and confirmatory tx carries a host record (platform id, OS,
     OS build, machine id, sender binary SHA-256; crates/sender/src/host.rs), the platform is
     registered, and all of them come from one (platform, OS, machine). OS build and binary
     hash changes are listed in the summary, not failed.
  L  ledger (`data/ledger.jsonl`, if present): every measure/calibrate reservation has a
     manifest line and vice versa, and the landed fees match.
Online (`--online`, RPC_URL from env; the key is never printed or stored):
  R  every saved block (or a seeded sample, `--sample K`) is re-fetched and must have the
     same blockhash, parent and signature list, in the same order.
  S  `getSlotLeaders` names the block's Fee-reward producer as the scheduled leader of every
     landed slot (RPC nodes keep schedules for recent epochs only; older slots are counted
     as unavailable, and the offline producer check B/X still holds).
  G  `getSignatureStatuses(searchTransactionHistory)`: landed sigs are finalized in the
     claimed slot; "not landed" sigs are unknown to the cluster.

Writes data/verify/verify_<UTC>.json and exits 1 if any check fails.
"""

from __future__ import annotations

import argparse
import datetime as dt
import gzip
import json
import os
import pathlib
import random
import re
import struct
import sys
import time
import urllib.error
import urllib.request
from collections import Counter, defaultdict

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

ROOT = pathlib.Path(__file__).resolve().parents[1]
ALPENGLOW_GENESIS_SLOT = 459_653_000  # crates/common/src/guard.rs
COMPUTE_BUDGET = "ComputeBudget111111111111111111111111111111"
LOADED_ACCOUNTS_DATA_LIMIT = 32 * 1024  # crates/sender/src/txbuild.rs
ORDERING_PURPOSES = ("measure", "calibrate")
B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
_IDX = {c: i for i, c in enumerate(B58)}


def b58decode(s: str) -> bytes:
    n = 0
    for c in s:
        n = n * 58 + _IDX[c]
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    return b"\0" * (len(s) - len(s.lstrip("1"))) + raw


def redact(s: str) -> str:
    return re.sub(r"(?i)((?:x-)?api[-_]?key=)[^&\s\"'<>#),;\\]+", r"\1REDACTED", s)


def read_jsonl(path: pathlib.Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(l) for l in path.read_text(encoding="utf-8").splitlines() if l.strip()]


class Report:
    def __init__(self) -> None:
        self.checked: Counter = Counter()
        self.problems: list[dict] = []

    def ok(self, check: str, n: int = 1) -> None:
        self.checked[check] += n

    def fail(self, check: str, what: str, **ctx) -> None:
        self.checked[check] += 1
        self.problems.append({"check": check, "problem": what, **ctx})


# ---------- block helpers ----------

def load_block(data_root: pathlib.Path, slot: int) -> dict | None:
    p = data_root / "raw" / "blocks" / f"{slot}.json.gz"
    if not p.exists():
        return None
    with gzip.open(p, "rt", encoding="utf-8") as f:
        return json.load(f)


def saved_slots(data_root: pathlib.Path) -> list[int]:
    d = data_root / "raw" / "blocks"
    return sorted(int(p.name.split(".")[0]) for p in d.glob("*.json.gz")) if d.exists() else []


def block_producer(block: dict) -> str | None:
    fees = [r["pubkey"] for r in block.get("rewards") or [] if r.get("rewardType") == "Fee"]
    return fees[0] if len(fees) == 1 else None


def first_sig(tx: dict) -> str:
    return tx["transaction"]["signatures"][0]


def compute_budget(msg: dict) -> dict:
    """Decode ComputeBudget instructions of a `json`-encoded message."""
    keys = msg["accountKeys"]
    out: dict = {}
    for ix in msg["instructions"]:
        if keys[ix["programIdIndex"]] != COMPUTE_BUDGET:
            continue
        d = b58decode(ix["data"])
        if not d:
            continue
        if d[0] == 2 and len(d) == 5:
            out["cu_limit"] = struct.unpack("<I", d[1:])[0]
        elif d[0] == 3 and len(d) == 9:
            out["cu_price"] = struct.unpack("<Q", d[1:])[0]
        elif d[0] == 4 and len(d) == 5:
            out["loaded_limit"] = struct.unpack("<I", d[1:])[0]
    return out


def writable_keys(msg: dict) -> list[str]:
    h, keys = msg["header"], msg["accountKeys"]
    signed, ro_signed, ro_unsigned = (h["numRequiredSignatures"], h["numReadonlySignedAccounts"],
                                      h["numReadonlyUnsignedAccounts"])
    w = keys[: signed - ro_signed]
    w += keys[signed: len(keys) - ro_unsigned]
    return w


def expected_exclusion(t: dict, slot: int, producer: str | None) -> str | None:
    """Same rule as crates/sender/src/collect.rs `exclusion` for a landed tx."""
    if producer is None:
        return "producer_unknown"
    if producer != t["identity"]:
        return "producer_mismatch"
    if slot < t["first_slot"] or slot >= t["first_slot"] + t["n_slots"]:
        return "outside_window"
    return None


# ---------- offline checks ----------

def load_schedules(data_root: pathlib.Path) -> list[dict]:
    out = []
    for p in sorted((data_root / "snapshots").glob("leader_schedule_epoch*.json.gz")):
        with gzip.open(p, "rt", encoding="utf-8") as f:
            d = json.load(f)
        # identity -> set of absolute slots
        d["by_slot"] = {d["first_slot"] + i: k for k, idx in d["schedule"].items() for i in idx}
        out.append(d)
    return out


def check_schedules(lines: list[dict], data_root: pathlib.Path, rep: Report) -> None:
    scheds = load_schedules(data_root)

    def leader(slot: int) -> tuple[bool, str | None]:
        for d in scheds:
            if d["first_slot"] <= slot < d["first_slot"] + d["slots_in_epoch"]:
                return True, d["by_slot"].get(slot)
        return False, None

    windows: dict[str, list[dict]] = defaultdict(list)
    for l in lines:
        windows[l["tx"]["window_id"]].append(l)
    for wid, ls in sorted(windows.items()):
        t = ls[0]["tx"]
        slots = set(range(t["first_slot"], t["first_slot"] + t["n_slots"]))
        slots |= {l["outcome"]["slot"] for l in ls if l["outcome"]["status"] == "landed"
                  and not l["outcome"].get("exclusion_reason")}
        bad = []
        for s in sorted(slots):
            have, who = leader(s)
            if not have:
                bad.append(f"{s}:no_snapshot")
            elif who != t["identity"]:
                bad.append(f"{s}:{who}")
        rep.fail("E", "not the scheduled leader: " + ",".join(bad), window_id=wid) if bad else rep.ok("E")


def send_order_ok(sends: list[dict], rank_of: dict[int, int]) -> bool:
    """True if the sends, ordered by actual start time, are in planned send_rank order."""
    done = [s for s in sends if s.get("error") is None]
    by_time = sorted(done, key=lambda s: (int(s["start_mono_ns"]), int(s.get("start_unix_ns", 0))))
    ranks = [rank_of[int(s["idx"])] for s in by_time]
    return ranks == sorted(ranks)


def check_send_order(lines: list[dict], rep: Report) -> None:
    """O: per window, actual send order (start_mono_ns) == planned send_rank order."""
    windows: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for l in lines:
        windows[(l["tx"]["run_id"], l["tx"]["window_id"])].append(l["tx"])
    for (run_id, wid), txs in sorted(windows.items()):
        txs = [t for t in txs if t.get("ordering_eligible", True)]
        if len(txs) < 2:
            continue
        ok = send_order_ok([dict(t["send"], idx=t["idx"]) for t in txs], {t["idx"]: t["send_rank"] for t in txs})
        rep.ok("O") if ok else rep.fail("O", "sent in a different order than send_rank", run_id=run_id, window_id=wid)


def check_send_order_windows(run_dir: pathlib.Path, rep: Report) -> None:
    """O on a dry-run (or live) run directory's windows.jsonl."""
    for w in read_jsonl(run_dir / "windows.jsonl"):
        sends = w.get("sends") or []
        if len(sends) < 2:
            continue
        rank_of = {t["idx"]: t["send_rank"] for t in w["window"]["design"]["txs"]}
        ok = send_order_ok(sends, rank_of)
        rep.ok("O") if ok else rep.fail("O", "sent in a different order than send_rank",
                                        run_id=w.get("run_id"), window_id=w["window"]["window_id"],
                                        arm=w["window"]["design"]["arm"])


def check_platform(lines: list[dict], rep: Report) -> dict:
    """Q: calibration and confirmatory windows ran on one registered machine."""
    keys: Counter = Counter()
    builds: Counter = Counter()
    hashes: Counter = Counter()
    for l in lines:
        t = l["tx"]
        if t.get("purpose") not in ORDERING_PURPOSES:
            continue
        h = t.get("host")
        if not h:
            rep.fail("Q", "no host record", sig=t["sig"])
            continue
        if h.get("platform") in (None, "", "unregistered") or h.get("machine_id") in (None, "", "unknown"):
            rep.fail("Q", f"not a registered platform: {h.get('platform')}/{h.get('machine_id')}", sig=t["sig"])
            continue
        keys[(h["platform"], h["os"], h["machine_id"])] += 1
        builds[h.get("os_build")] += 1
        hashes[h.get("sender_sha256")] += 1
        rep.ok("Q")
    if len(keys) > 1:
        rep.fail("Q", "calibration/confirmatory txs come from more than one platform",
                 platforms={"/".join(map(str, k)): n for k, n in sorted(keys.items())})
    return {"platforms": {"/".join(map(str, k)): n for k, n in sorted(keys.items())},
            "os_builds": dict(sorted(builds.items())), "sender_sha256": dict(sorted(hashes.items()))}


def check_plans(lines: list[dict], data_root: pathlib.Path, rep: Report) -> None:
    plans: dict[str, dict | None] = {}

    def planned(run_id: str) -> dict:
        if run_id not in plans:
            ws = [d["window"] for d in read_jsonl(data_root / "logs" / run_id / "plan.jsonl") if "window" in d]
            plans[run_id] = {w["window_id"]: w for w in ws}
        return plans[run_id]

    windows: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for l in lines:
        windows[(l["tx"]["run_id"], l["tx"]["window_id"])].append(l["tx"])
    for (run_id, wid), txs in sorted(windows.items()):
        w = planned(run_id).get(wid)
        if w is None:
            rep.fail("P", "window not in its run's plan.jsonl", run_id=run_id, window_id=wid)
            continue
        design = {d["idx"]: d for d in w["design"]["txs"]}
        bad = [f for f in ("identity", "first_slot", "n_slots") if any(t[f] != w[f] for t in txs)]
        if any(t["arm"] != w["design"]["arm"] for t in txs):
            bad.append("arm")
        for t in txs:
            d = design.get(t["idx"])
            if d is None or (d["cu_limit"], d["cu_price_micro_lamports"], d["send_rank"]) != (
                    t["cu_limit"], t["cu_price_micro_lamports"], t["send_rank"]):
                bad.append(f"tx{t['idx']}")
        rep.fail("P", "manifest differs from plan: " + ",".join(bad), window_id=wid) if bad else rep.ok("P")


def check_offline(lines: list[dict], data_root: pathlib.Path, ledger: list[dict], rep: Report) -> dict:
    sigs = Counter(l["tx"]["sig"] for l in lines)
    for s, c in sigs.items():
        if c > 1:
            rep.fail("M", "duplicate signature", sig=s, count=c)
    # our payers: from the manifest and from the ledger (which also covers dropped lines)
    payers = {l["tx"]["payer"] for l in lines} | {e["payer"] for e in ledger if e.get("payer")}
    # funding and sweep transfers have no manifest line by design
    transfer_sigs = {e["sig"] for e in ledger if e.get("kind") == "reserve" and e.get("purpose") not in ORDERING_PURPOSES}
    by_sig = {l["tx"]["sig"]: l for l in lines}
    cache: dict[int, dict | None] = {}

    def block(slot: int) -> dict | None:
        if slot not in cache:
            cache[slot] = load_block(data_root, slot)
        return cache[slot]

    for l in lines:
        t, o = l["tx"], l["outcome"]
        sig = t["sig"]
        if t.get("cluster") != "mainnet":
            rep.fail("M", "cluster is not mainnet", sig=sig, cluster=t.get("cluster"))
        elif t["first_slot"] < ALPENGLOW_GENESIS_SLOT or (o.get("slot") or ALPENGLOW_GENESIS_SLOT) < ALPENGLOW_GENESIS_SLOT:
            rep.fail("M", "slot before the Alpenglow guard", sig=sig, first_slot=t["first_slot"])
        else:
            rep.ok("M")
        if o["status"] != "landed":
            if o.get("slot") is not None or o.get("block_index") is not None:
                rep.fail("M", "not landed but has a position", sig=sig)
            continue
        slot, idx = o["slot"], o["block_index"]
        b = block(slot)
        if b is None:
            rep.fail("B", "saved block missing", sig=sig, slot=slot)
            continue
        txs = b["transactions"]
        if idx is None or idx >= len(txs) or first_sig(txs[idx]) != sig:
            rep.fail("B", "signature not at claimed index", sig=sig, slot=slot, block_index=idx)
            continue
        meta, msg = txs[idx]["meta"], txs[idx]["transaction"]["message"]
        prod = block_producer(b)
        bad = []
        if o.get("block_tx_count") is not None and o["block_tx_count"] != len(txs):
            bad.append("block_tx_count")
        if o.get("fee_paid") is not None and o["fee_paid"] != meta.get("fee"):
            bad.append("fee_paid")
        if "tx_error" in o and o["tx_error"] != meta.get("err"):
            bad.append("tx_error")
        if o.get("producer") != prod:
            bad.append("producer")
        rep.fail("B", "outcome differs from block: " + ",".join(bad), sig=sig, slot=slot) if bad else rep.ok("B")

        cb = compute_budget(msg)
        bad = []
        if msg["accountKeys"][0] != t["payer"]:
            bad.append("payer")
        if msg["recentBlockhash"] != t["blockhash"]:
            bad.append("blockhash")
        if msg["header"]["numRequiredSignatures"] != t["n_signatures"]:
            bad.append("n_signatures")
        if cb.get("cu_limit") != t["cu_limit"]:
            bad.append("cu_limit")
        if cb.get("cu_price", 0) != t["cu_price_micro_lamports"]:
            bad.append("cu_price")
        if cb.get("loaded_limit") != LOADED_ACCOUNTS_DATA_LIMIT:
            bad.append("loaded_accounts_data_limit")
        w = writable_keys(msg)
        expect_w = {t["payer"]} | ({t["conflict_account"]} if t.get("conflict_account") else set())
        if set(w) != expect_w:
            bad.append("writable_accounts")
        rep.fail("T", "tx differs from manifest: " + ",".join(bad), sig=sig, slot=slot) if bad else rep.ok("T")

        want = expected_exclusion(t, slot, prod)
        if o.get("exclusion_reason") != want:
            rep.fail("X", "exclusion_reason not reproduced", sig=sig, claimed=o.get("exclusion_reason"), derived=want)
        else:
            rep.ok("X")

    # H: scan every saved block for our signatures and our payers
    for slot in saved_slots(data_root):
        b = block(slot)
        for i, tx in enumerate(b["transactions"]):
            s = first_sig(tx)
            l = by_sig.get(s)
            if l is not None:
                o = l["outcome"]
                if o["status"] != "landed" or o.get("slot") != slot or o.get("block_index") != i:
                    rep.fail("H", "our tx is in a saved block but the manifest says otherwise",
                             sig=s, slot=slot, block_index=i, claimed_status=o["status"])
                else:
                    rep.ok("H")
            elif tx["transaction"]["message"]["accountKeys"][0] in payers:
                if s not in transfer_sigs:
                    rep.fail("H", "tx paid by our payer has no manifest line", sig=s, slot=slot)
        cache.pop(slot, None)

    # L: ledger <-> manifest
    if ledger:
        reserved = {e["sig"]: e for e in ledger if e.get("kind") == "reserve" and e.get("purpose") in ORDERING_PURPOSES}
        landed = {e["sig"]: e for e in ledger if e.get("kind") == "landed"}
        for s in reserved.keys() - by_sig.keys():
            rep.fail("L", "reserved (sent) but missing from the manifest", sig=s)
        for s in by_sig.keys() - reserved.keys():
            rep.fail("L", "manifest line without a ledger reservation", sig=s)
        for s, l in by_sig.items():
            if s not in reserved:
                continue
            o = l["outcome"]
            e = landed.get(s)
            if (o["status"] == "landed") != (e is not None):
                rep.fail("L", "landed status differs from ledger", sig=s)
            elif e is not None and (e["slot"] != o["slot"] or (o.get("fee_paid") is not None and e["fee"] != o["fee_paid"])):
                rep.fail("L", "slot or fee differs from ledger", sig=s)
            else:
                rep.ok("L")
    return {"txs": len(lines), "landed": sum(l["outcome"]["status"] == "landed" for l in lines),
            "payers": len(payers), "saved_blocks": len(saved_slots(data_root)),
            "ledger_entries": len(ledger)}


# ---------- online checks ----------

class Rpc:
    def __init__(self, url: str) -> None:
        from ratelimit import FileBucket
        self.url, self.limiter, self.calls = url, FileBucket(), Counter()

    def call(self, method: str, params: list, retries: int = 6):
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
        for attempt in range(retries):
            self.limiter.acquire()
            self.calls[method] += 1
            req = urllib.request.Request(self.url, body, {"content-type": "application/json"})
            try:
                with urllib.request.urlopen(req, timeout=120) as r:
                    resp = json.load(r)
            except urllib.error.HTTPError as e:
                if e.code == 429:
                    time.sleep(min(0.25 * 2**attempt, 8))
                    continue
                raise RuntimeError(f"{method}: HTTP {e.code}") from None
            except urllib.error.URLError as e:
                raise RuntimeError(f"{method}: {redact(str(e.reason))}") from None
            err = resp.get("error")
            if err and "Too many requests" in err.get("message", ""):
                time.sleep(min(0.25 * 2**attempt, 8))
                continue
            if err:
                raise RuntimeError(f"{method}: {redact(str(err))[:200]}")
            return resp["result"]
        raise RuntimeError(f"{method}: rate-limited after {retries} retries")


def check_online(lines: list[dict], data_root: pathlib.Path, rpc: Rpc, sample: int | None, seed: int,
                 rep: Report) -> dict:
    slots = saved_slots(data_root)
    if sample is not None and sample < len(slots):
        slots = sorted(random.Random(seed).sample(slots, sample))
    for slot in slots:
        saved = load_block(data_root, slot)
        fresh = rpc.call("getBlock", [slot, {"encoding": "json", "maxSupportedTransactionVersion": 1,
                                             "transactionDetails": "signatures", "rewards": True,
                                             "commitment": "finalized"}])
        bad = []
        if fresh.get("blockhash") != saved.get("blockhash"):
            bad.append("blockhash")
        if fresh.get("parentSlot") != saved.get("parentSlot"):
            bad.append("parentSlot")
        if fresh.get("signatures") != [first_sig(t) for t in saved["transactions"]]:
            bad.append("signature_order")
        if block_producer(fresh) != block_producer(saved):
            bad.append("producer")
        rep.fail("R", "saved block differs from the cluster: " + ",".join(bad), slot=slot) if bad else rep.ok("R")

    # S: the block's Fee-reward producer (used offline) is the scheduled leader. RPC nodes
    # serve leader schedules only for recent epochs; older slots are counted as unavailable.
    unavailable = 0
    for slot in sorted({l["outcome"]["slot"] for l in lines if l["outcome"]["status"] == "landed"}):
        try:
            leader = rpc.call("getSlotLeaders", [slot, 1])[0]
        except RuntimeError:
            unavailable += 1
            continue
        saved = load_block(data_root, slot)
        prod = block_producer(saved) if saved else None
        if leader != prod:
            rep.fail("S", "scheduled leader differs from the block's producer", slot=slot, leader=leader, producer=prod)
        else:
            rep.ok("S")

    for i in range(0, len(lines), 256):
        chunk = lines[i:i + 256]
        st = rpc.call("getSignatureStatuses", [[l["tx"]["sig"] for l in chunk], {"searchTransactionHistory": True}])
        for l, s in zip(chunk, st["value"]):
            o = l["outcome"]
            if o["status"] == "landed":
                if s is None or s.get("slot") != o["slot"] or s.get("confirmationStatus") != "finalized":
                    rep.fail("G", "landed sig not finalized in the claimed slot", sig=l["tx"]["sig"], status=s)
                else:
                    rep.ok("G")
            elif s is not None:
                rep.fail("G", "sig marked not landed is known to the cluster", sig=l["tx"]["sig"], status=s)
            else:
                rep.ok("G")
    return {"blocks_refetched": len(slots), "leader_schedule_unavailable_slots": unavailable,
            "rpc_calls": dict(rpc.calls)}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--data", default=str(ROOT / "data"))
    ap.add_argument("--manifest", action="append",
                    help="default: data/manifest.jsonl and data/exploratory/calibration_manifest.jsonl")
    ap.add_argument("--ledger", default=None, help="default: <data>/ledger.jsonl")
    ap.add_argument("--online", action="store_true", help="also re-fetch from RPC_URL (read-only)")
    ap.add_argument("--windows", action="append", help="also check send order in <run dir>/windows.jsonl")
    ap.add_argument("--sample", type=int, default=None, help="online: re-fetch only K saved blocks")
    ap.add_argument("--seed", type=int, default=20261023)
    ap.add_argument("--out", default=None, help="default: <data>/verify/verify_<UTC>.json")
    a = ap.parse_args(argv)

    data_root = pathlib.Path(a.data)
    manifests = [pathlib.Path(m) for m in a.manifest] if a.manifest else [
        data_root / "manifest.jsonl", data_root / "exploratory" / "calibration_manifest.jsonl"]
    lines = [l for m in manifests for l in read_jsonl(m)]
    ledger = read_jsonl(pathlib.Path(a.ledger) if a.ledger else data_root / "ledger.jsonl")
    rep = Report()
    summary = {"offline": check_offline(lines, data_root, ledger, rep)}
    check_plans(lines, data_root, rep)
    check_schedules(lines, data_root, rep)
    summary["platform"] = check_platform(lines, rep)
    check_send_order(lines, rep)
    for d in a.windows or []:
        check_send_order_windows(pathlib.Path(d), rep)
    url = os.environ.get("RPC_URL", "")
    if a.online:
        if not url:
            print("--online needs RPC_URL in the environment", file=sys.stderr)
            return 2
        summary["online"] = check_online(lines, data_root, Rpc(url), a.sample, a.seed, rep)
        summary["online"]["endpoint"] = redact(url)

    now = dt.datetime.now(dt.timezone.utc)
    out = {"label": "VERIFICATION", "generated_utc": now.strftime("%Y-%m-%dT%H:%M:%SZ"),
           "manifests": [str(m.relative_to(data_root)) if m.is_relative_to(data_root) else str(m) for m in manifests if m.exists()],
           "summary": summary, "checks_run": dict(sorted(rep.checked.items())),
           "problems": rep.problems, "ok": not rep.problems}
    out_path = pathlib.Path(a.out) if a.out else data_root / "verify" / f"verify_{now:%Y%m%dT%H%M%SZ}.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(out, indent=1) + "\n", encoding="utf-8")

    print(f"txs {summary['offline']['txs']}, landed {summary['offline']['landed']}, "
          f"saved blocks {summary['offline']['saved_blocks']}")
    for c, n in sorted(rep.checked.items()):
        bad = sum(p["check"] == c for p in rep.problems)
        print(f"  {c}: {n - bad}/{n} ok")
    for p in rep.problems[:20]:
        print("  FAIL", json.dumps(p))
    if len(rep.problems) > 20:
        print(f"  ... {len(rep.problems) - 20} more in {out_path}")
    print(("OK" if not rep.problems else f"{len(rep.problems)} PROBLEMS") + f" -> {out_path}")
    return 0 if not rep.problems else 1


if __name__ == "__main__":
    sys.exit(main())
