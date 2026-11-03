"""scripts/reproduce.py: the number-by-number comparison used by the one-command reproduction."""
import importlib.util
import math
import pathlib

ROOT = pathlib.Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("reproduce", ROOT / "scripts" / "reproduce.py")
rep = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(rep)

BASE = {"cells": [{"id": "a", "n": 5, "tau": 0.123456789012, "p": None, "ok": True, "x": [1.0, 2.0, float("nan")]}], "seed": 20261022}


def _copy():
    import copy
    return copy.deepcopy(BASE)


def test_identical():
    acc = rep.compare(BASE, _copy())
    assert acc["diffs"] == [] and acc["numbers"] == 6  # n, tau, 1.0, 2.0, nan, seed (None, bool, str are not numbers)


def test_float_noise_is_tolerated():
    new = _copy()
    new["cells"][0]["tau"] *= 1 + 1e-12
    assert rep.compare(BASE, new)["diffs"] == []


def test_real_difference_is_reported_with_its_path():
    new = _copy()
    new["cells"][0]["tau"] += 1e-6
    d = rep.compare(BASE, new)["diffs"]
    assert len(d) == 1 and d[0][0] == "$.cells[0].tau"


def test_integer_string_bool_and_none_must_match_exactly():
    for key, val in (("n", 6), ("id", "b"), ("ok", False), ("p", 0.5), ("seed", 1)):
        new = _copy()
        if key == "seed":
            new["seed"] = val
        else:
            new["cells"][0][key] = val
        assert rep.compare(BASE, new)["diffs"], key


def test_nan_matches_nan_only():
    new = _copy()
    new["cells"][0]["x"][2] = 1.0
    assert rep.compare(BASE, new)["diffs"]
    assert math.isnan(BASE["cells"][0]["x"][2])


def test_missing_key_and_list_length():
    new = _copy()
    del new["seed"]
    assert any("only in reference" in w for _, w in rep.compare(BASE, new)["diffs"])
    new = _copy()
    new["cells"].append({})
    assert any("list length" in w for _, w in rep.compare(BASE, new)["diffs"])
