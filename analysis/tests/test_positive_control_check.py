"""scripts/positive_control_check.py on SYNTHETIC results jsons; alloc_v2.json is a simulation file."""
import copy
import importlib.util
import json
import pathlib

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("positive_control_check", ROOT / "scripts" / "positive_control_check.py")
pc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pc)

ALLOC = {
    "positive_control_agave_C": {"W": 15, "interval": {"union": [0.65, 1.0]}},
    "scenarios": {"calibrated": {"positive_control_agave_C": {"W": 15, "interval": {"union": [0.63, 0.99]}}},
                  "wifi_sensitivity": {"positive_control_agave_C": {"W": 10, "interval": {"union": [0.58, 1.0]}}}},
}


def _cells(a=(20, 0.80), b=(10, 0.50)):
    res = {"cells": [
        {"identity": "A", "arm": "C", "stratum": "Agave", "windows_usable": a[0], "rules": {"R_RATIO_AGAVE": {"mean_tau": a[1]}}},
        {"identity": "B", "arm": "C", "stratum": "Agave", "windows_usable": b[0], "rules": {"R_RATIO_AGAVE": {"mean_tau": b[1]}}},
        {"identity": "A", "arm": "P", "stratum": "Agave", "windows_usable": 5, "rules": {"R_RATIO_AGAVE": {"mean_tau": -1.0}}},
        {"identity": "Z", "arm": "C", "stratum": "Firedancer", "windows_usable": 5, "rules": {"R_RATIO_AGAVE": {"mean_tau": -1.0}}},
    ]}
    pooled = {"pooled_stratum": "Agave", "cells": [{"arm": "C", "by_validator_descriptive": {
        "A": {"windows_usable": a[0], "mean_tau": {"R_RATIO_AGAVE": a[1]}},
        "B": {"windows_usable": b[0], "mean_tau": {"R_RATIO_AGAVE": b[1]}}}}]}
    return res, pooled


def test_primary_interval_is_the_union_of_top_level_and_calibrated():
    iv = pc.intervals(ALLOC)
    assert iv["primary"]["interval"] == [0.63, 1.0]
    assert iv["preset_only"]["interval"] == [0.65, 1.0] and iv["wifi_sensitivity"]["interval"] == [0.58, 1.0]
    assert iv["wifi_sensitivity"]["W"] == 10


def test_statistic_is_the_plain_mean_over_all_windows_of_the_agave_arm_c_cells_only():
    res, pooled = _cells()
    ob = pc.observed(res, pooled)
    assert ob["usable_windows"] == 30
    assert ob["mean_raw_tau"] == pytest.approx((20 * 0.80 + 10 * 0.50) / 30)  # not the mean of unit means (0.65)


def test_pass_and_fail_with_bounds_included():
    res, pooled = _cells()
    assert pc.evaluate(res, pooled, ALLOC)["pass"]
    res, pooled = _cells(a=(20, 0.40), b=(10, 0.40))
    assert not pc.evaluate(res, pooled, ALLOC)["pass"]
    res, pooled = _cells(a=(10, 0.63), b=(10, 0.63))
    assert pc.evaluate(res, pooled, ALLOC)["pass"]  # on the lower bound: inside


def test_stops_when_the_two_files_disagree_or_nothing_is_usable():
    res, pooled = _cells()
    bad = copy.deepcopy(pooled)
    bad["cells"][0]["by_validator_descriptive"]["A"]["windows_usable"] = 19
    with pytest.raises(SystemExit, match="disagree"):
        pc.observed(res, bad)
    res, pooled = _cells(a=(0, None), b=(0, None))
    with pytest.raises(SystemExit, match="cannot be evaluated"):
        pc.observed(res, pooled)


def test_cli_reads_files_and_the_real_alloc_file_is_consistent(tmp_path, capsys):
    res, pooled = _cells(a=(20, 0.40), b=(10, 0.40))
    for n, o in (("r.json", res), ("p.json", pooled)):
        (tmp_path / n).write_text(json.dumps(o), encoding="utf-8")
    alloc_path = ROOT / "analysis" / "sim" / "results" / "alloc_v2.json"
    assert pc.main(["--results", str(tmp_path / "r.json"), "--pooled", str(tmp_path / "p.json"), "--alloc", str(alloc_path)]) == 1
    out = capsys.readouterr().out
    assert "FAILED" in out and "primary" in out and "preset_only" in out and "wifi_sensitivity" in out
    alloc = json.loads(alloc_path.read_text(encoding="utf-8"))
    iv = pc.intervals(alloc)["primary"]["interval"]
    assert iv[0] == min(alloc["positive_control_agave_C"]["interval"]["union"][0],
                        alloc["scenarios"]["calibrated"]["positive_control_agave_C"]["interval"]["union"][0])
