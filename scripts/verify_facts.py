#!/usr/bin/env python3
"""Re-derive every on-chain fact in docs/VERIFIED_FACTS.md from mainnet RPC.

Read-only: never signs or sends a transaction. Stdlib only, so it runs anywhere.

    RPC_URL=<endpoint from your provider> python scripts/verify_facts.py

The endpoint comes only from the environment. Its API key is never printed or stored:
the snapshot records the URL with `api-key=REDACTED`. Requests are paced to <= 8/s, and
429s are retried with exponential backoff (same policy as crates/sender/src/rpc.rs).

Writes data/snapshots/facts_<UTC timestamp>.json (+ gzipped raw cluster/vote/Kobe
snapshots) and prints a short summary. Exits non-zero if a checked fact no longer holds.
"""

from __future__ import annotations

import base64
import collections
import datetime as dt
import gzip
import itertools
import json
import os
import pathlib
import re
import struct
import sys
import time
import urllib.error
import urllib.request

RPC_URL = os.environ.get("RPC_URL", "")
MIN_INTERVAL_S = 1 / 8
_last_call = 0.0


def redact(s: str) -> str:
    return re.sub(r"(?i)((?:x-)?api[-_]?key=)[^&\s\"'<>#),;\\]+", r"\1REDACTED", s)
KOBE_URL = os.environ.get("KOBE_URL", "https://kobe.mainnet.jito.network/api/v1/validators")
ROOT = pathlib.Path(__file__).resolve().parents[1]
OUT_DIR = ROOT / "data" / "snapshots"

ALPENGLOW_GENESIS_SLOT = 459_653_000
VOTE_PROGRAM = "Vote111111111111111111111111111111111111111"
B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58decode(s: str) -> bytes:
    n = 0
    for c in s:
        n = n * 58 + B58.index(c)
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return b"\0" * (len(s) - len(s.lstrip("1"))) + raw


def rpc(method: str, params: list | None = None, retries: int = 6):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params or []}).encode()
    global _last_call
    for attempt in range(retries):
        wait = _last_call + MIN_INTERVAL_S - time.monotonic()
        if wait > 0:
            time.sleep(wait)
        _last_call = time.monotonic()
        req = urllib.request.Request(RPC_URL, body, {"content-type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
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
        return resp
    raise RuntimeError(f"{method}: rate-limited after {retries} retries")


def result(method, params=None):
    resp = rpc(method, params)
    if "error" in resp:
        raise RuntimeError(f"{method}{params}: {resp['error']}")
    return resp["result"]


def block_summary(slot: int) -> dict:
    resp = rpc(
        "getBlock",
        [slot, {"maxSupportedTransactionVersion": 1, "transactionDetails": "accounts", "rewards": True}],
    )
    if "error" in resp:
        return {"slot": slot, "error": resp["error"]["message"][:120]}
    b = resp["result"]
    txs = b["transactions"]
    vote = sum(
        1 for t in txs if any(k["pubkey"] == VOTE_PROGRAM for k in t["transaction"]["accountKeys"])
    )
    fee_rewards = [r for r in (b.get("rewards") or []) if r.get("rewardType") == "Fee"]
    versions = collections.Counter(str(t.get("version")) for t in txs)
    return {
        "slot": slot,
        "parentSlot": b["parentSlot"],
        "blockTime": b["blockTime"],
        "n_tx": len(txs),
        "n_vote_tx": vote,
        "tx_versions": dict(versions),
        "fee_reward_entries": len(fee_rewards),
        "fee_reward_pubkey": fee_rewards[0]["pubkey"] if fee_rewards else None,
    }


def fee_message(blockhash: bytes, price: int, limit: int) -> str:
    """Legacy message: SetComputeUnitLimit, SetComputeUnitPrice, Memo. Random payer, never signed."""
    payer = os.urandom(32)
    cb = b58decode("ComputeBudget111111111111111111111111111111")
    memo = b58decode("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr")
    keys = [payer, cb, memo]
    ixs = [
        (1, b"\x02" + struct.pack("<I", limit)),
        (1, b"\x03" + struct.pack("<Q", price)),
        (2, b"x"),
    ]
    out = bytes([1, 0, 2, len(keys)]) + b"".join(keys) + blockhash + bytes([len(ixs)])
    for prog, data in ixs:
        out += bytes([prog, 0, len(data)]) + data
    return base64.b64encode(out).decode()


def http_json(url: str):
    req = urllib.request.Request(url, headers={"user-agent": "solana-leader-window-lab/verify_facts"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def main() -> int:
    now = dt.datetime.now(dt.timezone.utc)
    stamp = now.strftime("%Y%m%dT%H%M%SZ")
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    if not RPC_URL:
        print("RPC_URL is not set (environment only; see .env.example)", file=sys.stderr)
        return 2
    facts: dict = {"generated_at_utc": now.isoformat(), "rpc_url": redact(RPC_URL)}
    failures: list[str] = []

    facts["getVersion"] = result("getVersion")
    facts["getEpochSchedule"] = result("getEpochSchedule")
    facts["getEpochInfo"] = result("getEpochInfo")

    # F1: Alpenglow boundary: last block with vote txs and first block after it.
    produced = result("getBlocksWithLimit", [ALPENGLOW_GENESIS_SLOT - 2, 12])
    before = [s for s in produced if s < ALPENGLOW_GENESIS_SLOT]
    after = [s for s in produced if s >= ALPENGLOW_GENESIS_SLOT]
    facts["boundary"] = {
        "produced_slots_near_boundary": produced,
        "last_block_before": block_summary(before[-1]) if before else None,
        "first_block_after": block_summary(after[0]) if after else None,
    }
    lb, fa = facts["boundary"]["last_block_before"], facts["boundary"]["first_block_after"]
    if not (lb and lb.get("n_vote_tx", 0) > 0):
        failures.append("F1: expected vote txs in the last pre-boundary block")
    if not (fa and fa.get("n_vote_tx", 1) == 0):
        failures.append("F1: expected zero vote txs in the first post-boundary block")

    # F2: slot time, from block times of produced slots.
    tip = facts["getEpochInfo"]["absoluteSlot"]
    samples = {}
    for s in [ALPENGLOW_GENESIS_SLOT + 50_000, tip - 100_000, tip - 2_000]:
        got = result("getBlocksWithLimit", [s, 1])
        if got:
            samples[got[0]] = result("getBlockTime", [got[0]])
    ks = sorted(samples)
    facts["slot_time"] = {
        "samples": samples,
        "ms_per_slot_long": 1000 * (samples[ks[-1]] - samples[ks[0]]) / (ks[-1] - ks[0]),
        "performance_samples": result("getRecentPerformanceSamples", [5]),
    }

    # F3: leader window length (consecutive slots per leader) and alignment.
    epoch_first = facts["getEpochInfo"]["absoluteSlot"] - facts["getEpochInfo"]["slotIndex"]
    leaders = result("getSlotLeaders", [epoch_first, 5000])
    runs = [len(list(g)) for _, g in itertools.groupby(leaders)]
    starts = [i for i in range(len(leaders)) if i == 0 or leaders[i] != leaders[i - 1]]
    facts["leader_windows"] = {
        "from_slot": epoch_first,
        "run_length_counts": dict(collections.Counter(runs)),
        "run_start_mod_4": dict(collections.Counter((epoch_first + s) % 4 for s in starts)),
    }

    # F4: blockhash validity and rent-exempt minimum.
    bh = rpc("getLatestBlockhash", [{"commitment": "processed"}])["result"]
    height = result("getBlockHeight", [{"commitment": "processed"}])
    facts["blockhash"] = {
        "lastValidBlockHeight": bh["value"]["lastValidBlockHeight"],
        "blockHeight_now": height,
        "validity_blocks_remaining": bh["value"]["lastValidBlockHeight"] - height,
    }
    facts["rent_exempt_min_0_bytes"] = result("getMinimumBalanceForRentExemption", [0])

    # F5: fee formula, via getFeeForMessage.
    blockhash = b58decode(bh["value"]["blockhash"])
    fee_cases = []
    for price, limit in [(0, 1000), (1, 1000), (999, 1000), (1000, 1000), (1001, 1000), (1_000_000, 1000)]:
        val = result("getFeeForMessage", [fee_message(blockhash, price, limit), {"commitment": "processed"}])["value"]
        expect = 5000 + -(-price * limit // 1_000_000)
        fee_cases.append({"cu_price": price, "cu_limit": limit, "fee": val, "formula": expect})
        if val != expect:
            failures.append(f"F5: fee mismatch {price=} {limit=} rpc={val} formula={expect}")
    facts["fee_for_message"] = fee_cases

    # F7: client labels from gossip clientId, weighted by stake.
    nodes = result("getClusterNodes")
    votes = result("getVoteAccounts")
    stake = {v["nodePubkey"]: v["activatedStake"] for v in votes["current"]}
    total = sum(stake.values())
    by = collections.defaultdict(lambda: {"nodes": 0, "voting_nodes": 0, "stake": 0})
    for n in nodes:
        key = f'{n.get("clientId")}|{n.get("version")}'
        by[key]["nodes"] += 1
        if n["pubkey"] in stake:
            by[key]["voting_nodes"] += 1
            by[key]["stake"] += stake[n["pubkey"]]
    facts["clients_by_stake"] = sorted(
        ({"client|version": k, **v, "stake_pct": round(100 * v["stake"] / total, 3)} for k, v in by.items()),
        key=lambda r: -r["stake"],
    )
    try:
        kobe = http_json(KOBE_URL)
        vals = kobe.get("validators", [])
        facts["kobe"] = {
            "url": KOBE_URL,
            "n_validators": len(vals),
            "fields": sorted(vals[0].keys()) if vals else [],
            "running_jito": sum(1 for v in vals if v.get("running_jito")),
            "running_bam": sum(1 for v in vals if v.get("running_bam")),
        }
    except Exception as e:  # optional source; record the failure instead of aborting
        kobe = None
        facts["kobe"] = {"url": KOBE_URL, "error": repr(e)}

    for name, obj in [("cluster_nodes", nodes), ("vote_accounts", votes), ("kobe_validators", kobe)]:
        if obj is not None:
            with gzip.open(OUT_DIR / f"{name}_{stamp}.json.gz", "wt") as f:
                json.dump(obj, f)

    facts["failures"] = failures
    out = OUT_DIR / f"facts_{stamp}.json"
    out.write_text(json.dumps(facts, indent=2))
    print(f"wrote {out.relative_to(ROOT)}")
    print(json.dumps({k: facts[k] for k in ["boundary", "leader_windows", "blockhash", "rent_exempt_min_0_bytes"]}, indent=1))
    print("ms_per_slot_long:", round(facts["slot_time"]["ms_per_slot_long"], 1))
    for row in facts["clients_by_stake"][:12]:
        print(f'{row["stake_pct"]:7.3f}%  {row["voting_nodes"]:4d} voting  {row["client|version"]}')
    if failures:
        print("FAILURES:", *failures, sep="\n  ", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
