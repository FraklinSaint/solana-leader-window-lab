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
