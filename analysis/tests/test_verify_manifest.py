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
                "sig": sig, "run_id": "live-20261101T000000Z", "cluster": "mainnet", "purpose": "measure", "host": dict(HOST),
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
    write_blocklist(tmp, windows)
    return lines, ledger


def write_blocklist(tmp, windows, skip=None, finalized=10 ** 9, commitment="finalized"):
    """logs/<run>/blocklist.json as slwlab.blocklist writes it; `skip`: {window index: slot offset}."""
    skip = skip or {}
    slots = {k: [s for s in range(460_200_000 + 8 * k, 460_200_000 + 8 * k + 4)
                 if skip.get(k) != s - (460_200_000 + 8 * k)] for k in range(windows)}
    rec = {"label": "SLOT LIST (getBlocks)", "method": "getBlocks", "commitment": commitment,
           "run_id": "live-20261101T000000Z", "fetched_utc": "2026-11-01T01:00:00Z",
           "finalized_slot_at_fetch": finalized,
           "calls": [{"start": 460_200_000, "end": 460_200_000 + 8 * windows, "n_blocks": 0}],
           "windows": {f"{460_200_000 + 8 * k}-{k}": {"first_slot": 460_200_000 + 8 * k, "n_slots": 4,
                                                     "slots_with_blocks": v} for k, v in slots.items()}}
    p = tmp / "logs" / "live-20261101T000000Z" / "blocklist.json"
    p.write_text(json.dumps(rec), encoding="utf-8")
    return rec


HOST = {"platform": "win-pc1", "os": "windows", "arch": "x86_64", "os_build": "10.0.17763.6893",
        "machine_id": "0123456789abcdef", "sender_sha256": "ab" * 32}


def run(tmp, lines, ledger):
    rep = vm.Report()
    vm.check_offline(lines, tmp, ledger, rep)
    vm.check_plans(lines, tmp, rep)
    vm.check_schedules(lines, tmp, rep)
    vm.check_platform(lines, rep)
    vm.check_send_order(lines, rep)
    vm.check_blocklists(lines, tmp, rep)
    return rep


def test_clean_data_passes(tmp_path):
    for arm in ("P", "C"):
        lines, ledger = build(tmp_path / arm, arm=arm)
        rep = run(tmp_path / arm, lines, ledger)
        assert rep.problems == [], rep.problems
        assert {"M", "B", "T", "P", "E", "X", "H", "L", "Q", "O", "K"} <= set(rep.checked)


def test_a_tiny_block_cache_gives_the_same_verdict_and_stays_bounded(tmp_path):
    """The block cache is an LRU: ~1800 blocks do not fit in a 7 GB CI runner (measured >6 GB unbounded)."""
    lines, ledger = build(tmp_path, windows=6)
    full = run(tmp_path, lines, ledger)
    old, vm.BLOCK_CACHE_MAX = vm.BLOCK_CACHE_MAX, 1
    try:
        small = run(tmp_path, lines, ledger)
        lines[0]["outcome"]["block_index"] += 1  # a real failure is still found with the tiny cache
        bad = run(tmp_path, lines, ledger)
    finally:
        vm.BLOCK_CACHE_MAX = old
    assert full.problems == [] and small.problems == [] and dict(full.checked) == dict(small.checked)
    assert {"B", "H"} <= {p["check"] for p in bad.problems}


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
    # P: manifest differs from the plan; O: the recorded send times no longer follow send_rank
    assert checks_failed(rep) == {"P", "O"}
    lines[0]["tx"]["run_id"] = "live-other"
    assert "window not in its run's plan.jsonl" in {p["problem"] for p in run(tmp_path, lines, ledger).problems}


def test_unregistered_host_other_machine_and_missing_host_are_caught(tmp_path):
    lines, ledger = build(tmp_path)
    lines[0]["tx"]["host"] = dict(HOST, platform="unregistered")
    lines[1]["tx"]["host"] = dict(HOST, machine_id="fedcba9876543210")
    del lines[2]["tx"]["host"]
    lines[3]["tx"]["host"] = dict(HOST, os_build="10.0.17763.7000", sender_sha256="cd" * 32)
    rep = vm.Report()
    summary = vm.check_platform(lines, rep)
    probs = [p["problem"] for p in rep.problems]
    assert any("not a registered platform" in p for p in probs)
    assert any("no host record" in p for p in probs)
    assert any("more than one platform" in p for p in probs)
    # an OS update or a rebuilt binary on the same machine is reported, not failed
    assert len(summary["os_builds"]) == 2 and len(summary["sender_sha256"]) == 2
    assert len(probs) == 3


def test_send_order_must_follow_send_rank(tmp_path):
    lines, ledger = build(tmp_path)
    # the fixture sends in send_rank order: clean
    rep = vm.Report()
    vm.check_send_order(lines, rep)
    assert rep.problems == [] and rep.checked["O"] > 0
    # idx order instead of rank order (the 2026-10-23 burst bug) is caught
    win = lines[0]["tx"]["window_id"]
    same = [l["tx"] for l in lines if l["tx"]["window_id"] == win]
    for t in same:
        t["send_rank"] = (len(same) - 1) - t["idx"]
        t["send"]["start_mono_ns"] = t["idx"] * 1000
    rep = vm.Report()
    vm.check_send_order(lines, rep)
    assert any(p["window_id"] == win for p in rep.problems)


def test_send_order_in_dryrun_windows(tmp_path):
    d = tmp_path / "dryrun-20261023T130118Z"
    d.mkdir()

    def rec(wid, ranks, mono):
        return {"run_id": d.name, "window": {"window_id": wid, "design": {"arm": "P", "txs": [
            {"idx": i, "send_rank": r} for i, r in enumerate(ranks)]}},
                "sends": [{"idx": i, "start_mono_ns": m, "start_unix_ns": 0, "error": None} for i, m in enumerate(mono)]}
    good = rec("1-good", [2, 0, 1], [3000, 1000, 2000])
    bad = rec("2-bad", [2, 0, 1], [1000, 2000, 3000])  # idx order
    (d / "windows.jsonl").write_text(json.dumps(good) + "\n" + json.dumps(bad) + "\n")
    rep = vm.Report()
    vm.check_send_order_windows(d, rep)
    assert [p["window_id"] for p in rep.problems] == ["2-bad"]


def test_a_second_network_type_is_a_second_stratum(tmp_path):
    lines, ledger = build(tmp_path)
    for l in lines:
        l["tx"]["host"] = dict(HOST, network_type="wifi")
    rep = vm.Report()
    summary = vm.check_platform(lines, rep)
    assert not rep.problems and summary["network_types"] == {"wifi": len(lines)}
    # rows from before the field existed and an unreadable adapter are listed, never failed
    lines[0]["tx"]["host"] = dict(HOST)
    lines[1]["tx"]["host"] = dict(HOST, network_type="unknown")
    rep = vm.Report()
    summary = vm.check_platform(lines, rep)
    assert not rep.problems and summary["network_types"]["unknown"] == 2
    lines[2]["tx"]["host"] = dict(HOST, network_type="ethernet")
    rep = vm.Report()
    vm.check_platform(lines, rep)
    assert any("more than one network type" in p["problem"] for p in rep.problems)


def test_k_missing_list_fails_for_measure_but_not_for_calibrate(tmp_path):
    lines, ledger = build(tmp_path)
    (tmp_path / "logs" / "live-20261101T000000Z" / "blocklist.json").unlink()
    rep = run(tmp_path, lines, ledger)
    assert checks_failed(rep) == {"K"}
    for l in lines:
        l["tx"]["purpose"] = "calibrate"
    rep = vm.Report()
    summary = vm.check_blocklists(lines, tmp_path, rep)
    assert rep.problems == [] and summary["runs_without_list_calibrate"] == ["live-20261101T000000Z"]


def test_k_skipped_slot_is_fine_unless_it_contradicts_our_blocks(tmp_path):
    lines, ledger = build(tmp_path)
    # window 1: slot offset 3 is skipped on chain; our txs landed in offset 0 (the block we hold)
    write_blocklist(tmp_path, 4, skip={1: 3})
    rep = vm.Report()
    summary = vm.check_blocklists(lines, tmp_path, rep)
    assert rep.problems == [] and summary["windows_with_skipped_slot"] == 1
    # a list that says the slot we landed in has no block is wrong and must be caught
    write_blocklist(tmp_path, 4, skip={1: 0})
    rep = vm.Report()
    vm.check_blocklists(lines, tmp_path, rep)
    assert checks_failed(rep) == {"K"}


def test_k_unfinalized_and_wrong_commitment_are_caught(tmp_path):
    lines, ledger = build(tmp_path)
    write_blocklist(tmp_path, 4, finalized=460_200_005)
    rep = vm.Report()
    vm.check_blocklists(lines, tmp_path, rep)
    assert checks_failed(rep) == {"K"}
    write_blocklist(tmp_path, 4, commitment="confirmed")
    rep = vm.Report()
    vm.check_blocklists(lines, tmp_path, rep)
    assert checks_failed(rep) == {"K"}


def test_k_online_refetch_must_match(tmp_path):
    lines, ledger = build(tmp_path)
    rec = write_blocklist(tmp_path, 4, skip={2: 1})
    have = sorted(s for w in rec["windows"].values() for s in w["slots_with_blocks"])

    class FakeRpc:
        def __init__(self, extra=()):
            self.extra = set(extra)

        def call(self, method, params):
            assert method == "getBlocks"
            return sorted(set(have) | self.extra)

    rep = vm.Report()
    vm.check_blocklists_online(lines, tmp_path, FakeRpc(), rep)
    assert rep.problems == [] and rep.checked["K"] == 4
    rep = vm.Report()
    vm.check_blocklists_online(lines, tmp_path, FakeRpc(extra=[460_200_000 + 8 * 2 + 1]), rep)
    assert checks_failed(rep) == {"K"}
