"""scripts/verify_manifest.py on a SYNTHETIC manifest + blocks (tmp dir, never data/).
Blocks use the same `json` encoding as getBlock, with real ComputeBudget instruction data."""

import gzip
import importlib.util
import json
import pathlib
import struct

import numpy as np

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("verify_manifest", ROOT / "scripts" / "verify_manifest.py")
vm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(vm)

B58 = vm.B58
IDENT = "Va1idatorIdentity1111111111111111111111111"
SYSTEM = "11111111111111111111111111111111"
CB = vm.COMPUTE_BUDGET


def b58encode(b: bytes) -> str:
    n = int.from_bytes(b, "big")
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = B58[r] + s
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + s


def key(rng) -> str:
    return b58encode(bytes(rng.integers(1, 256, 32, dtype=np.uint8)))


def our_tx(sig, payer, blockhash, cu_limit, price, sink=None):
    keys = [payer] + ([sink] if sink else []) + [SYSTEM, CB]
    cb = len(keys) - 1
    ixs = [{"programIdIndex": cb, "accounts": [], "data": b58encode(b"\x02" + struct.pack("<I", cu_limit))},
           {"programIdIndex": cb, "accounts": [], "data": b58encode(b"\x03" + struct.pack("<Q", price))},
           {"programIdIndex": cb, "accounts": [], "data": b58encode(b"\x04" + struct.pack("<I", 32 * 1024))},
           {"programIdIndex": cb - 1, "accounts": [0, 1 if sink else 0], "data": "3Bxs4Bc3VYuGVB19"}]
    return {"transaction": {"signatures": [sig], "message": {
        "header": {"numRequiredSignatures": 1, "numReadonlySignedAccounts": 0, "numReadonlyUnsignedAccounts": 2},
        "accountKeys": keys, "recentBlockhash": blockhash, "instructions": ixs}},
        "meta": {"fee": 5000 + price * cu_limit // 1_000_000, "err": None}}


def other_tx(rng):
    return {"transaction": {"signatures": [key(rng)], "message": {
        "header": {"numRequiredSignatures": 1, "numReadonlySignedAccounts": 0, "numReadonlyUnsignedAccounts": 1},
        "accountKeys": [key(rng), SYSTEM], "recentBlockhash": "h", "instructions": []}},
        "meta": {"fee": 5000, "err": None}}


def build(tmp, windows=4, n=6, arm="P", seed=0):
    rng = np.random.default_rng(seed)
    lines, ledger, plan = [], [], []
    for k in range(windows):
        slot = 460_200_000 + 8 * k
        bh = key(rng)
        sink = key(rng) if arm == "C" else None
        block = [other_tx(rng) for _ in range(40)]
        pos = sorted(rng.choice(40, n - 1, replace=False))  # last tx does not land
        for i in range(n):
            sig, payer = key(rng) + key(rng)[:20], key(rng)
            price, limit = int(rng.integers(1, 5000)), int(rng.integers(300, 1000))
            landed = i < n - 1
            tx = our_tx(sig, payer, bh, limit, price, sink)
            if landed:
                block[int(pos[i])] = tx
            lines.append({"tx": {
                "sig": sig, "run_id": "live-20261101T000000Z", "cluster": "mainnet", "purpose": "measure",
                "window_id": f"{slot}-{k}", "identity": IDENT, "vote_account": "v", "stratum": "Agave",
                "arm": arm, "gap_us": 0, "offset_ms": -100, "first_slot": slot, "n_slots": 4, "idx": i,
                "send_rank": i, "payer": payer, "cu_limit": limit, "cu_price_micro_lamports": price,
                "n_signatures": 1, "conflict_account": sink, "ordering_eligible": True,
                "reserved_lamports": 20000, "blockhash": bh, "last_valid_block_height": 1,
                "send": {"path": "quic", "planned_offset_us": 0, "start_mono_ns": 0}},
                "outcome": {"status": "landed" if landed else "expired", "slot": slot if landed else None,
                            "block_index": None, "block_tx_count": 40 if landed else None,
                            "producer": IDENT if landed else None,
                            "fee_paid": tx["meta"]["fee"] if landed else None, "tx_error": None,
                            "exclusion_reason": None if landed else "not_landed", "resolved_at": "t"}})
            ledger.append({"kind": "reserve", "sig": sig, "purpose": "measure", "amount": 20000, "payer": payer})
            if landed:
                ledger.append({"kind": "landed", "sig": sig, "slot": slot, "fee": tx["meta"]["fee"]})
        for l in lines:
            if l["outcome"]["slot"] == slot:
                l["outcome"]["block_index"] = next(j for j, t in enumerate(block) if t["transaction"]["signatures"][0] == l["tx"]["sig"])
        plan.append({"window": {"window_id": f"{slot}-{k}", "identity": IDENT, "first_slot": slot, "n_slots": 4,
                                "design": {"arm": arm, "txs": [
                                    {"idx": l["tx"]["idx"], "cu_limit": l["tx"]["cu_limit"],
                                     "cu_price_micro_lamports": l["tx"]["cu_price_micro_lamports"],
                                     "send_rank": l["tx"]["send_rank"]}
                                    for l in lines if l["tx"]["window_id"] == f"{slot}-{k}"]}}})
        d = tmp / "raw" / "blocks"
        d.mkdir(parents=True, exist_ok=True)
        with gzip.open(d / f"{slot}.json.gz", "wt", encoding="utf-8") as f:
            json.dump({"blockhash": key(rng), "parentSlot": slot - 1, "transactions": block,
                       "rewards": [{"rewardType": "Fee", "pubkey": IDENT, "lamports": 1}]}, f)
    first = 460_080_000  # epoch 1065
    sched = {IDENT: [s - first + j for s in range(460_200_000, 460_200_000 + 8 * windows, 8) for j in range(4)],
             "OtherLeader1111111111111111111111111111111": [s - first + j for s in range(460_200_004, 460_200_000 + 8 * windows, 8) for j in range(4)]}
    (tmp / "snapshots").mkdir(parents=True, exist_ok=True)
    with gzip.open(tmp / "snapshots" / "leader_schedule_epoch1065.json.gz", "wt", encoding="utf-8") as f:
        json.dump({"label": "SNAPSHOT", "epoch": 1065, "first_slot": first, "slots_in_epoch": 432_000,
                   "schedule": sched}, f)
    logs = tmp / "logs" / "live-20261101T000000Z"
    logs.mkdir(parents=True, exist_ok=True)
    (logs / "plan.jsonl").write_text("".join(json.dumps(p) + "\n" for p in plan))
    return lines, ledger


def run(tmp, lines, ledger):
    rep = vm.Report()
    vm.check_offline(lines, tmp, ledger, rep)
    vm.check_plans(lines, tmp, rep)
    vm.check_schedules(lines, tmp, rep)
    return rep


def test_clean_data_passes(tmp_path):
    for arm in ("P", "C"):
        lines, ledger = build(tmp_path / arm, arm=arm)
        rep = run(tmp_path / arm, lines, ledger)
        assert rep.problems == [], rep.problems
        assert {"M", "B", "T", "P", "E", "X", "H", "L"} <= set(rep.checked)


def test_main_writes_report_and_exit_code(tmp_path):
    lines, ledger = build(tmp_path)
    (tmp_path / "manifest.jsonl").write_text("".join(json.dumps(l) + "\n" for l in lines))
    (tmp_path / "ledger.jsonl").write_text("".join(json.dumps(e) + "\n" for e in ledger))
    out = tmp_path / "v.json"
    assert vm.main(["--data", str(tmp_path), "--out", str(out)]) == 0
    assert json.loads(out.read_text())["ok"] is True
    lines[0]["outcome"]["block_index"] += 1
    (tmp_path / "manifest.jsonl").write_text("".join(json.dumps(l) + "\n" for l in lines))
    assert vm.main(["--data", str(tmp_path), "--out", str(out)]) == 1


def first_landed(lines):
    return next(l for l in lines if l["outcome"]["status"] == "landed")


def checks_failed(rep):
    return {p["check"] for p in rep.problems}


def test_claimed_price_must_match_chain(tmp_path):
    lines, ledger = build(tmp_path)
    first_landed(lines)["tx"]["cu_price_micro_lamports"] += 1
    rep = run(tmp_path, lines, ledger)
    assert "T" in checks_failed(rep) and "cu_price" in rep.problems[0]["problem"]


def test_moved_position_and_hidden_landing_are_caught(tmp_path):
    lines, ledger = build(tmp_path)
    first_landed(lines)["outcome"]["block_index"] += 1
    assert {"B", "H"} <= checks_failed(run(tmp_path, lines, ledger))

    lines, ledger = build(tmp_path, seed=1)
    l = first_landed(lines)
    l["outcome"].update(status="expired", slot=None, block_index=None, exclusion_reason="not_landed")
    assert "H" in checks_failed(run(tmp_path, lines, ledger))


def test_dropped_tx_and_wrong_exclusion_are_caught(tmp_path):
    lines, ledger = build(tmp_path)
    dropped = lines.pop(0)  # a sent tx silently removed from the manifest
    rep = run(tmp_path, lines, ledger)
    assert any(p["check"] == "L" and p["sig"] == dropped["tx"]["sig"] for p in rep.problems)
    assert any(p["check"] == "H" and p["sig"] == dropped["tx"]["sig"] for p in rep.problems)

    lines, ledger = build(tmp_path, seed=2)
    first_landed(lines)["tx"]["first_slot"] -= 100  # landed outside its window, claimed clean
    assert "X" in checks_failed(run(tmp_path, lines, ledger))


def test_window_of_a_non_scheduled_leader_is_caught(tmp_path):
    lines, ledger = build(tmp_path)
    for l in lines:
        if l["tx"]["window_id"] == lines[0]["tx"]["window_id"]:
            l["tx"]["first_slot"] += 4  # the 4 slots that belong to OtherLeader
    rep = run(tmp_path, lines, ledger)
    assert any(p["check"] == "E" and "OtherLeader" in p["problem"] for p in rep.problems)
    (tmp_path / "snapshots" / "leader_schedule_epoch1065.json.gz").unlink()
    assert any("no_snapshot" in p["problem"] for p in run(tmp_path, lines, ledger).problems)


def test_pre_alpenglow_and_non_mainnet_are_rejected(tmp_path):
    lines, ledger = build(tmp_path)
    lines[0]["tx"]["cluster"] = "devnet"
    lines[1]["tx"]["first_slot"] = vm.ALPENGLOW_GENESIS_SLOT - 1
    assert sum(p["check"] == "M" for p in run(tmp_path, lines, ledger).problems) == 2


def test_extra_writable_account_outside_arm_c_is_caught(tmp_path):
    lines, ledger = build(tmp_path, arm="C")
    first_landed(lines)["tx"]["conflict_account"] = None
    assert "T" in checks_failed(run(tmp_path, lines, ledger))


def test_online_checks_against_a_fake_cluster(tmp_path):
    lines, ledger = build(tmp_path)

    class FakeRpc:
        calls: dict = {}

        def __init__(self, tamper=False):
            self.tamper = tamper

        def call(self, method, params):
            if method == "getBlock":
                b = vm.load_block(tmp_path, params[0])
                sigs = [t["transaction"]["signatures"][0] for t in b["transactions"]]
                if self.tamper:
                    sigs[0], sigs[1] = sigs[1], sigs[0]
                return {"blockhash": b["blockhash"], "parentSlot": b["parentSlot"], "signatures": sigs,
                        "rewards": b["rewards"]}
            if method == "getSlotLeaders":
                return [IDENT]
            if method == "getSignatureStatuses":
                by = {l["tx"]["sig"]: l["outcome"] for l in lines}
                return {"value": [{"slot": by[s]["slot"], "confirmationStatus": "finalized"}
                                  if by[s]["status"] == "landed" else None for s in params[0]]}
            raise AssertionError(method)

    rep = vm.Report()
    vm.check_online(lines, tmp_path, FakeRpc(), None, 0, rep)
    assert rep.problems == [] and {"R", "S", "G"} <= set(rep.checked)
    rep = vm.Report()
    vm.check_online(lines, tmp_path, FakeRpc(tamper=True), 2, 0, rep)
    assert checks_failed(rep) == {"R"} and rep.checked["R"] == 2


def test_send_order_changed_after_planning_is_caught(tmp_path):
    lines, ledger = build(tmp_path)
    a, b = lines[0]["tx"], lines[1]["tx"]
    a["send_rank"], b["send_rank"] = b["send_rank"], a["send_rank"]
    rep = run(tmp_path, lines, ledger)
    assert checks_failed(rep) == {"P"}
    lines[0]["tx"]["run_id"] = "live-other"
    assert "window not in its run's plan.jsonl" in {p["problem"] for p in run(tmp_path, lines, ledger).problems}
