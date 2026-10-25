"""calibrate.py recovers the nuisance used to generate a SYNTHETIC calibration set (tmp dir
only, labeled SIMULATION) and flags values outside the preset range."""

import json

import numpy as np

from slwlab import calibrate, sim

from test_parse_analyze import synth


def write_cal(tmp_path, lines, errs):
    (tmp_path / "exploratory").mkdir(parents=True, exist_ok=True)
    with open(tmp_path / "exploratory" / "calibration_manifest.jsonl", "w") as f:
        for l in lines:
            l["tx"]["purpose"] = "calibrate"
            f.write(json.dumps(l) + "\n")
    (tmp_path / "logs" / "dryrun-x").mkdir(parents=True, exist_ok=True)
    with open(tmp_path / "logs" / "dryrun-x" / "clock.jsonl", "w") as f:
        for e in errs:
            f.write(json.dumps({"err_prep_ms": float(e)}) + "\n")


def test_landing_clock_and_deviation_flags(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=40, seed=7)
    errs = np.random.default_rng(1).normal(0, 80, 500)
    write_cal(tmp_path, lines, errs)
    est = calibrate.estimate(tmp_path)
    e = est["estimates"]
    assert abs(e["p_land"]["value"] - 0.9) < 0.05  # synth uses MEDIUM p_land = 0.9
    assert 60 < e["clock_sd_ms"]["value"] < 100
    assert e["clock_sd_ms"]["deviation"] is True  # 80 ms is outside the preset [100, 100]
    assert e["jitter_ms"]["value"] is None
    nz = calibrate.calibrated_nuisance(est)
    assert nz.p_land == e["p_land"]["value"]


def test_ordering_is_never_read(tmp_path, monkeypatch):
    # Shuffling every block_index within its window must not change any estimate.
    lines = synth(tmp_path, sim.AGAVE, windows=30, seed=9)
    write_cal(tmp_path, lines, [0.0, 10.0, -10.0])
    a = calibrate.estimate(tmp_path)
    rng = np.random.default_rng(0)
    by_w = {}
    for l in lines:
        if l["outcome"]["block_index"] is not None:
            by_w.setdefault(l["tx"]["window_id"], []).append(l)
    for ls in by_w.values():
        idx = [l["outcome"]["block_index"] for l in ls]
        for l, i in zip(ls, rng.permutation(idx)):
            l["outcome"]["block_index"] = int(i)
    write_cal(tmp_path, lines, [0.0, 10.0, -10.0])
    b = calibrate.estimate(tmp_path)
    assert {k: v["value"] for k, v in a["estimates"].items()} == {k: v["value"] for k, v in b["estimates"].items()}


def write_session(tmp_path, name, mode, rtts, start=1_000):
    d = tmp_path / "sessions" / name
    d.mkdir(parents=True, exist_ok=True)
    (d / "session.json").write_text(json.dumps({"id": name, "mode": mode}))
    with open(d / "rtt.jsonl", "w") as f:
        f.write(json.dumps({"unix": start - 5, "rtt_ms": 900.0, "warmup": True}) + "\n")
        for i, r in enumerate(rtts):
            row = {"unix": start + 5 * i}
            row.update({"rtt_ms": r} if r is not None else {"error": "timeout"})
            f.write(json.dumps(row) + "\n")


def test_rtt_stats_match_the_rust_definitions():
    # same vectors as netmon.rs tests: 10,20,10,20 at 5 s -> jitter 10, nearest-rank median 10
    s = [(i * 5, r) for i, r in enumerate([10.0, 20.0, 10.0, 20.0])]
    st = calibrate.rtt_stats(s)
    assert st["jitter_ms"] == 10.0 and st["p50_ms"] == 10.0 and st["n_pairs"] == 3
    # MAD sigma of 10..18: median 14, MAD 2 -> 2.9652
    st = calibrate.rtt_stats([(i * 5, 10.0 + i) for i in range(9)])
    assert abs(st["sigma_mad_ms"] - 2.9652) < 1e-9
    # a failed probe and a 90 s gap make no pair; loss is counted
    st = calibrate.rtt_stats([(0, 10.0), (5, None), (10, 30.0), (100, 50.0), (105, 50.0)])
    assert st["n_err"] == 1 and st["n_pairs"] == 1 and st["jitter_ms"] == 0.0
    assert calibrate.rtt_stats([])["p50_ms"] is None


def test_network_path_uses_calibration_sessions_only_and_flags_a_wifi_sensitivity(tmp_path):
    rng = np.random.default_rng(3)
    # RTT sd about 12 ms -> one-way proxy about 8.5 ms, above the preset high of 3 ms
    write_session(tmp_path, "20261025T180000Z", "calibrate", list(40 + rng.normal(0, 12, 400)))
    write_session(tmp_path, "20261025T120000Z", "dryrun", [500.0] * 50)  # not calibration
    np_ = calibrate.network_path(tmp_path)
    assert np_["sessions"] == ["20261025T180000Z"]
    assert np_["rtt"]["n"] == 400, "the warmup probe is excluded"
    assert 6 < np_["oneway_jitter_proxy_ms"] < 11
    assert np_["proxy_above_preset_high"] is True
    # data without sessions: a well-formed empty record
    empty = calibrate.network_path(tmp_path / "nothing")
    assert empty["rtt"]["n"] == 0 and empty["proxy_above_preset_high"] is False


def test_estimate_carries_the_path_record_and_never_overrides_jitter(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=20, seed=7)
    write_cal(tmp_path, lines, np.random.default_rng(1).normal(0, 80, 200))
    write_session(tmp_path, "20261025T180000Z", "calibrate",
                  list(40 + np.random.default_rng(4).normal(0, 12, 300)))
    est = calibrate.estimate(tmp_path)
    assert est["network_path"]["proxy_above_preset_high"] is True
    j = est["estimates"]["jitter_ms"]
    assert j["value"] is None and j["deviation"] is False
    assert calibrate.calibrated_nuisance(est).jitter_ms == sim.MEDIUM.jitter_ms
    wifi = calibrate.wifi_sensitivity_nuisance(est)
    assert wifi is not None and wifi.jitter_ms == est["network_path"]["oneway_jitter_proxy_ms"]
    # a quiet path needs no extra simulator run
    est["network_path"]["proxy_above_preset_high"] = False
    assert calibrate.wifi_sensitivity_nuisance(est) is None
