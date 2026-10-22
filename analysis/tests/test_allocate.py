"""allocate.py: the pre-registered allocation respects the budget and never trims required cells."""

import json
import pathlib

from slwlab import allocate, sim

RES = pathlib.Path(__file__).resolve().parents[1] / "sim" / "results" / "alloc_v1.json"


def test_allocation_within_budget_and_required_untrimmed():
    curves = json.loads(RES.read_text())["curves"]
    a = allocate.allocate(curves, sim.MEDIUM)
    assert a["within_budget"] and a["confirmatory_total_lamports"] <= allocate.CONFIRMATORY_BUDGET
    req = [r for r in a["rows"] if r["tier"] == "required"]
    assert len(req) == 3 * 5 and not any(r["trimmed"] for r in req)
    assert all(r["power_target_reached"] for r in req)
    assert all(r["W_usable_per_validator"] % 5 == 0 for r in a["rows"] if r["arm"] == "X")
    bonus_arms = {r["arm"] for r in a["rows"] if r["tier"] == "bonus"}
    assert bonus_arms <= {"P", "X", "C", "F"}


def test_agave_lock_free_needs_more_windows_than_firedancer():
    curves = json.loads(RES.read_text())["curves"]
    a = allocate.allocate(curves, sim.MEDIUM)
    w = {(r["group"], r["arm"]): r["W_usable_per_validator"] for r in a["rows"]}
    assert w[("Agave", "P")] > w[("Firedancer", "P")] and w[("Agave", "R")] > w[("Firedancer", "R")]
