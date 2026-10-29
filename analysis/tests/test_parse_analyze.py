"""parse.py + analyze.py end to end on a SYNTHETIC manifest built from the simulator.
Test fixture only (tmp dir): never written to data/, labeled SIMULATION."""

import datetime as dt
import gzip
import json

import numpy as np

from slwlab import analyze, parse, sim
from slwlab.model import ArmConfig

IDENT = "Va1idatorIdentity1111111111111111111111111"
B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def fake_key(rng):
    return "".join(rng.choice(list(B58), 44))


def synth(tmp_path, scheduler, arm="P", windows=30, seed=0, stratum="Agave", ordering=True):
    rng = np.random.default_rng(seed)
    lines = []
    for k in range(windows):
        w = sim.simulate_window(arm, scheduler, sim.MEDIUM, rng, ArmConfig())
        slot = 460_200_000 + 10 * k
        other = int(rng.integers(50, 500))
        pos = sorted(rng.choice(other, size=len(w.landed), replace=False))
        index = {i: int(p) for i, p in zip(w.landed, pos)}
        block_txs = [{"transaction": {"signatures": [fake_key(rng)]}, "meta": {"fee": 5000}} for _ in range(other)]
        sigs = {}
        for t in w.design.txs:
            sigs[t.idx] = fake_key(rng)
            if t.idx in index:
                block_txs[index[t.idx]]["transaction"]["signatures"][0] = sigs[t.idx]
        (tmp_path / "raw" / "blocks").mkdir(parents=True, exist_ok=True)
        with gzip.open(tmp_path / "raw" / "blocks" / f"{slot}.json.gz", "wt") as f:
            json.dump({"transactions": block_txs, "rewards": [{"rewardType": "Fee", "pubkey": IDENT}]}, f)
        for t in w.design.txs:
            landed = t.idx in index
            lines.append({"tx": {
                "sig": sigs[t.idx], "run_id": "live-20261101T000000Z", "cluster": "mainnet",
                "purpose": "measure", "window_id": f"{slot}-{k}", "identity": IDENT, "vote_account": "v",
                "stratum": stratum, "arm": arm, "gap_us": w.design.gap_us, "offset_ms": -100,
                "first_slot": slot, "n_slots": 4, "idx": t.idx, "send_rank": t.send_rank,
                "payer": fake_key(rng), "cu_limit": t.cu_limit, "cu_price_micro_lamports": t.price,
                "n_signatures": 1, "conflict_account": "S" if t.conflict else None,
                "ordering_eligible": ordering,
                "send": {"path": "quic", "planned_offset_us": t.send_offset_us,
                         "start_mono_ns": int(w.arrival_us[t.idx] * 1000)}},
                "outcome": {"status": "landed" if landed else "expired", "slot": slot if landed else None,
                            "block_index": index.get(t.idx), "producer": IDENT if landed else None,
                            "exclusion_reason": None if landed else "not_landed"}})
    return lines


TH = {"alpha": 0.05, "window_tau_norm": 0.5, "strict_mean": 0.8, "strict_ci_lower": 0.6,
      "strict_window_share": 0.5}
SIM_REF = {"seed": 1,
           "ceilings": {"agave": {"P": {r: {"medium": {"mean_tau": 0.45}} for r in
                                        ("R_PRICE", "R_TOTAL", "R_RATIO_AGAVE", "R_RATIO_FD", "R_FBA50")}}},
           "cells": {f"agave/P/{r}/N6/W{w}": {"thresholds": TH, "power_half": 0.95, "p_strict_half": 0.0}
                     for r in ("R_PRICE", "R_TOTAL", "R_RATIO_AGAVE", "R_RATIO_FD", "R_FBA50", "R_FIFO")
                     for w in (10, 20)}}


def test_agave_like_data_is_detected_and_positions_verify(tmp_path):
    lines = synth(tmp_path, sim.AGAVE)
    ws = parse.windows_from_manifest(lines)
    usable = [w for w in ws if w.usable]
    assert len(usable) >= 20
    for w in usable:
        assert parse.verify_positions(w, tmp_path) == []
    res = analyze.analyze(lines, SIM_REF)
    cell = res["cells"][0]
    r = cell["rules"]["R_RATIO_AGAVE"]
    assert r["p_holm"] < 0.01 and r["verdict"] in ("STRICT_RULE", "SOFT_RULE")
    assert cell["rules"]["R_FIFO"]["verdict"] in ("NO_RULE", "INCONCLUSIVE")
    assert res["label"] == "MEASUREMENT" and res["simulation_ref"]["label"] == "SIMULATION"


def test_random_data_finds_no_rule(tmp_path):
    lines = synth(tmp_path, sim.RANDOM, seed=3)
    cell = analyze.analyze(lines, SIM_REF)["cells"][0]
    assert all(v["verdict"] in ("NO_RULE", "INCONCLUSIVE") for v in cell["rules"].values())


def test_delivery_check_windows_are_never_usable(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=3, ordering=False)
    assert all(w.reason == "delivery_check_path" for w in parse.windows_from_manifest(lines))


def test_tampered_position_is_caught(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=6)
    target = next(w for w in parse.windows_from_manifest(lines) if w.usable).window_id
    for l in lines:
        if l["tx"]["window_id"] == target and l["outcome"]["block_index"] is not None:
            l["outcome"]["block_index"] += 1
            break
    probs = [p for w in parse.windows_from_manifest(lines) if w.usable for p in parse.verify_positions(w, tmp_path)]
    assert probs


def test_window_sent_out_of_send_rank_order_is_excluded(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=3)
    wid = lines[0]["tx"]["window_id"]
    for l in lines:
        if l["tx"]["window_id"] == wid:  # idx order instead of send_rank order (bug of 2026-10-23)
            l["tx"]["send"]["start_mono_ns"] = l["tx"]["idx"] * 1000
            l["tx"]["send_rank"] = 5 - l["tx"]["idx"]
    ws = {w.window_id: w for w in parse.windows_from_manifest(lines)}
    assert ws[wid].reason == "send_order_mismatch" and not ws[wid].usable
    assert all(w.reason != "send_order_mismatch" for k, w in ws.items() if k != wid)


def test_window_date_is_the_utc_day_of_the_send_time_not_of_the_run_start(tmp_path):
    """A run started on Oct 28 21:35Z crosses 00:00Z: its windows after midnight are Oct 29."""
    ns = lambda d, h: int(dt.datetime(2026, 10, d, h, 0, tzinfo=dt.timezone.utc).timestamp() * 1e9)
    lines = synth(tmp_path, sim.AGAVE, windows=2)
    for l in lines:
        l["tx"]["run_id"] = "live-20261028T213507Z"
    for k, l in enumerate(lines):
        l["tx"]["send"]["start_unix_ns"] = ns(28, 23) if l["tx"]["window_id"].endswith("-0") else ns(29, 1)
    ws = {w.window_id.rsplit("-", 1)[1]: w for w in parse.windows_from_manifest(lines)}
    assert ws["0"].date == "20261028" and ws["1"].date == "20261029"
    assert ws["0"].run_date == ws["1"].run_date == "20261028"
    # a send error has no send time and must not set it
    for l in lines:
        if l["tx"]["window_id"].endswith("-1"):
            l["tx"]["send"]["error"] = "x"
    assert {w.window_id.rsplit("-", 1)[1]: w for w in parse.windows_from_manifest(lines)}["1"].send_unix_ns is None
    # no send time at all: falls back to the day in the run id
    for l in lines:
        l["tx"]["send"].pop("start_unix_ns")
    assert all(w.date == "20261028" for w in parse.windows_from_manifest(lines))
