#!/usr/bin/env python3
"""Explain a `sender reconcile` gap (READ-ONLY, stdlib only; changes no data, sends nothing).

    python scripts/explain_reconcile_gap.py --before data/snapshots/balances_A.json --after data/snapshots/balances_B.json

`sender reconcile` demands: sum(wallets before) - sum(wallets after) == fees of the ledger's landed
txs in (before.slot, after.slot]. An incoming transfer from a foreign wallet (dust, a gift) breaks
that equality without any missing fee. This script checks it for EVERY account in the snapshots:

 1. lists the account's finalized signatures with slot in (before.slot, after.slot]
    (getSignaturesForAddress, paged);
 2. splits them into ledger txs (sig in data/ledger.jsonl `landed`) and the rest;
 3. reads each tx's balance change of that account (saved block in data/raw/blocks, else
    getTransaction for that one signature) and its fee payer;
 4. checks per account   wallet_delta == sum(pre-post of ledger txs) + sum(pre-post of other txs)
    (chain completeness: no tx is missing from the list), and globally
        total_wallet_delta + foreign_inflow == ledger_fees        (EXACT, or the script exits 1).

Other txs are printed with signature, sender address, slot and amount; a sender that is our own
master or sink is marked OURS (an unrecorded tx of ours), anything else FOREIGN. The RPC url comes
from the environment (RPC_URL) or .env and is never printed. It computes no outcome statistic.
"""

from __future__ import annotations

import argparse
import gzip
import json
import os
import pathlib
import sys
import time
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]


def rpc_url() -> str:
    u = os.environ.get("RPC_URL")
    if not u:
        for line in (ROOT / ".env").read_text(encoding="utf-8").splitlines():
            if line.startswith("RPC_URL="):
                u = line.split("=", 1)[1].strip().strip('"')
    if not u:
        raise SystemExit("RPC_URL is not set (environment or .env)")
    return u


class Rpc:
    def __init__(self, url: str):
        self.url, self.calls, self.last = url, 0, 0.0

    def __call__(self, method: str, params: list):
        wait = 0.15 - (time.monotonic() - self.last)  # about 6 calls a second, under the 8 rps bucket
        if wait > 0:
            time.sleep(wait)
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
        req = urllib.request.Request(self.url, body, {"content-type": "application/json"})
        for attempt in range(4):
            try:
                with urllib.request.urlopen(req, timeout=30) as r:
                    out = json.load(r)
                break
            except Exception as e:  # never print the url
                if attempt == 3:
                    raise SystemExit(f"{method} failed: {type(e).__name__}")
                time.sleep(1 + attempt)
        self.last = time.monotonic()
        self.calls += 1
        if "error" in out:
            raise SystemExit(f"{method} error: {str(out['error'])[:120]}")
        return out["result"]


def read_jsonl(p: pathlib.Path):
    for l in p.read_text(encoding="utf-8").splitlines():
        if l.strip():
            yield json.loads(l)


def tx_balances(tx: dict) -> tuple[list[str], list[int], list[int], int]:
    keys = [k if isinstance(k, str) else k["pubkey"] for k in tx["transaction"]["message"]["accountKeys"]]
    m = tx["meta"]
    return keys, m["preBalances"], m["postBalances"], m["fee"]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--before", required=True)
    ap.add_argument("--after", required=True)
    ap.add_argument("--data", default=str(ROOT / "data"))
    ap.add_argument("--label", default="")
    a = ap.parse_args()
    data = pathlib.Path(a.data)
    b = json.loads(pathlib.Path(a.before).read_text(encoding="utf-8"))
    f = json.loads(pathlib.Path(a.after).read_text(encoding="utf-8"))
    lo, hi = b["slot"], f["slot"]
    ours = set(b["accounts"])
    kinds = {k: v[0] for k, v in b["accounts"].items()}
    internal = {k for k, t in kinds.items() if t in ("master", "sink")}

    ledger_fee: dict[str, tuple[int, int]] = {}  # sig -> (fee, slot) for landed in range
    for e in read_jsonl(data / "ledger.jsonl"):
        if e["kind"] == "landed" and lo < e["slot"] <= hi:
            ledger_fee[e["sig"]] = (e["fee"], e["slot"])
    rpc = Rpc(rpc_url())
    cache: dict[str, dict | None] = {}

    def get_tx(sig: str, slot: int | None) -> dict:
        if sig in cache:
            return cache[sig]
        t = None
        if slot is not None:
            p = data / "raw" / "blocks" / f"{slot}.json.gz"
            if p.exists():
                blk = json.load(gzip.open(p, "rt", encoding="utf-8"))
                for x in blk["transactions"]:
                    if x["transaction"]["signatures"][0] == sig:
                        t = x
                        break
        if t is None:
            t = rpc("getTransaction", [sig, {"encoding": "json", "maxSupportedTransactionVersion": 1,
                                             "commitment": "finalized"}])
        cache[sig] = t
        return t

    print(f"== {a.label} before slot {lo} ({b['ts']}) -> after slot {hi} ({f['ts']}); "
          f"{len(ledger_fee)} ledger txs landed in range, {len(ours)} accounts")
    tot_delta = tot_foreign_in = tot_ours_in = tot_ext_net = 0
    problems = []
    n_match = n_internal = 0
    foreign_rows = []
    seen_other: dict[str, dict] = {}
    for acct in sorted(ours):
        delta = b["accounts"][acct][1] - f["accounts"][acct][1]
        sigs, before = [], None
        while True:
            page = rpc("getSignaturesForAddress", [acct, {"limit": 1000, "commitment": "finalized",
                                                          **({"before": before} if before else {})}])
            sigs += page
            if len(page) < 1000 or page[-1]["slot"] <= lo:
                break
            before = page[-1]["signature"]
        inr = [s for s in sigs if lo < s["slot"] <= hi]
        led_chg = oth_chg = fee_paid = foreign_in = ours_in = 0
        for s in inr:
            t = get_tx(s["signature"], s["slot"])
            keys, pre, post, fee = tx_balances(t)
            i = keys.index(acct)
            chg = pre[i] - post[i]
            if s["signature"] in ledger_fee:
                led_chg += chg
                if i == 0:
                    fee_paid += fee
            else:
                oth_chg += chg
                if s["signature"] not in seen_other:
                    seen_other[s["signature"]] = t
                # sender = every account whose balance fell other than ours-as-receiver; fee payer first
                senders = [keys[j] for j in range(len(keys)) if pre[j] - post[j] > 0 and keys[j] != acct] or [keys[0]]
                mine = -chg
                kind = "OURS" if any(x in internal for x in senders) else "FOREIGN"
                if mine > 0:
                    if kind == "FOREIGN":
                        foreign_in += mine
                    else:
                        ours_in += mine
                foreign_rows.append((acct, s["signature"], ",".join(senders), s["slot"], mine, kind, s.get("err")))
        led_fee_acct = sum(ledger_fee[s["signature"]][0] for s in inr if s["signature"] in ledger_fee
                           and tx_balances(get_tx(s["signature"], s["slot"]))[0][0] == acct)
        complete = delta == led_chg + oth_chg
        tot_delta += delta
        tot_ext_net += -oth_chg
        tot_foreign_in += foreign_in
        tot_ours_in += ours_in
        flag = "" if delta == led_fee_acct else "  <- delta != own ledger fees"
        if not complete:
            problems.append(f"{acct}: wallet delta {delta} != chain sum {led_chg + oth_chg} (a tx is missing from the list?)")
        if not flag:
            n_match += 1
        elif complete and not oth_chg:
            n_internal += 1  # differs from its own fees only by the 1-lamport conflict transfers to the sink
        if not complete or oth_chg:
            print(f"  {kinds[acct]:6s} {acct[:8]}.. delta {delta:8d} ledger_fees {led_fee_acct:8d} "
                  f"ledger-tx change {led_chg:8d} other-tx change {oth_chg:8d} complete={complete}{flag}")
    fees_total = sum(v[0] for v in ledger_fee.values())
    print(f"  {n_match} accounts: delta == own ledger fees; {n_internal} accounts: differ only by our own internal transfers (chain sum complete)")
    print("-- transactions touching our accounts that are NOT in the ledger (sig, sender, slot, amount into our account):")
    if not foreign_rows:
        print("   none")
    for acct, sig, snd, slot, amt, kind, err in foreign_rows:
        print(f"   {kind:7s} acct {acct[:8]}.. sig {sig} sender {snd} slot {slot} amount {amt} err {err}")
    print(f"-- totals: wallet delta {tot_delta}, ledger fees {fees_total}, foreign inflow {tot_foreign_in}, "
          f"inflow from our own wallets {tot_ours_in}, net external effect {tot_ext_net}")
    exact = tot_delta + tot_ext_net == fees_total
    print(f"-- wallet delta + external inflow == ledger fees: {tot_delta + tot_ext_net} == {fees_total} -> "
          f"{'EXACT' if exact else 'NOT EXACT'};  plain reconcile equality (delta == fees): {tot_delta == fees_total}")
    print(f"-- {rpc.calls} RPC calls")
    for p in problems:
        print("PROBLEM:", p)
    if tot_ours_in:
        print("WARNING: inflow from our own wallets that is not in the ledger = an unrecorded tx of ours")
    return 0 if exact and not problems and not tot_ours_in else 1


if __name__ == "__main__":
    sys.exit(main())
