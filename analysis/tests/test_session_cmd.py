"""scripts/session_cmd.py: the session cap comes from the allocation file and the ledger."""
import importlib.util
import json
import pathlib
import shutil

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("session_cmd", ROOT / "scripts" / "session_cmd.py")
sc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sc)

SEL = sc.newest_selection(ROOT / "data")


def _data(tmp_path):
    (tmp_path / "snapshots").mkdir(parents=True)
    shutil.copy(SEL, tmp_path / "snapshots" / SEL.name)
    return tmp_path


def _agave():
    sel = json.loads(SEL.read_text(encoding="utf-8"))
    return next(p["identity"] for p in sel["picked"] if p["stratum"] == "Agave")


def _log_windows(data, run_id, ident, arm, n, txs=3):
    (data / "logs" / run_id).mkdir(parents=True, exist_ok=True)
    plan, led = [], []
    for i in range(n):
        wid = f"{run_id}-{ident[:4]}-{arm}-{i}"
        plan.append({"window": {"window_id": wid, "identity": ident, "design": {"arm": arm}}})
        for t in range(txs):  # several reservations, one window
            led.append({"kind": "reserve", "run_id": run_id, "sig": f"{wid}-{t}", "amount": 5000,
                        "purpose": "measure", "window_id": wid})
    (data / "logs" / run_id / "plan.jsonl").write_text("".join(json.dumps(x) + "\n" for x in plan), encoding="utf-8")
    with open(data / "ledger.jsonl", "a", encoding="utf-8") as f:
        f.write("".join(json.dumps(x) + "\n" for x in led))


def test_session_1_with_nothing_done(tmp_path):
    r = sc.compute(ROOT, _data(tmp_path), 1, 8)
    assert r["remaining_cost"] == 7_569_190 and r["remaining_windows"] == 239
    assert r["cap"] == 7_947_650  # ceil(7,569,190 x 1.05)
    assert r["payer_target"] == 650_240 + 248_365 + 6_600
    assert r["carry_over_cost"] == 0
    assert round(r["line_per_hour"]) == 993_456 and 31 < r["windows_per_hour"] < 32


def test_cap_is_proportional_to_the_hours(tmp_path):
    d = _data(tmp_path)
    c8, c6 = sc.compute(ROOT, d, 1, 8)["cap"], sc.compute(ROOT, d, 1, 6)["cap"]
    assert c6 == -(-7_569_190 * 105 * 6 // (100 * 8))
    assert abs(c6 - 0.75 * c8) <= 1
    # the line per hour stays the same
    assert sc.compute(ROOT, d, 1, 6)["line_per_hour"] == sc.compute(ROOT, d, 1, 8)["line_per_hour"]


def test_done_windows_are_subtracted_once_per_window(tmp_path):
    d = _data(tmp_path)
    ident = _agave()
    _log_windows(d, "live-20261102T100000Z", ident, "P", 5)
    r = sc.compute(ROOT, d, 1, 8)
    alloc = json.loads(sc.alloc_path(ROOT, 1, 4).read_text(encoding="utf-8"))["allocation"]
    assert r["windows_done"] == 5 and r["remaining_windows"] == 234
    assert r["remaining_cost"] == 7_569_190 - 5 * alloc["spend_per_window_lamports"]["P"]


def test_a_shortfall_carries_into_the_next_session(tmp_path):
    d = _data(tmp_path)
    r2 = sc.compute(ROOT, d, 2, 8)  # session 1 did nothing
    assert r2["remaining_cost"] == 14_346_268 and r2["carry_over_cost"] == 7_569_190
    _log_windows(d, "live-20261102T100000Z", _agave(), "P", 4)
    r2b = sc.compute(ROOT, d, 2, 8)
    assert r2b["remaining_cost"] < r2["remaining_cost"] and r2b["windows_done"] == 4


def test_a_reserved_window_missing_from_its_plan_is_an_error(tmp_path):
    d = _data(tmp_path)
    (d / "logs" / "live-x").mkdir(parents=True)
    (d / "logs" / "live-x" / "plan.jsonl").write_text("", encoding="utf-8")
    (d / "ledger.jsonl").write_text(json.dumps({"kind": "reserve", "run_id": "live-x", "sig": "s", "amount": 1,
                                               "purpose": "measure", "window_id": "w"}) + "\n", encoding="utf-8")
    with pytest.raises(SystemExit, match="not in logs"):
        sc.compute(ROOT, d, 1, 8)


def test_committed_counts_landed_fees_and_open_reservations_only(tmp_path):
    d = _data(tmp_path)
    rows = [
        {"kind": "reserve", "run_id": "r", "sig": "a", "amount": 100, "purpose": "measure"},
        {"kind": "landed", "sig": "a", "fee": 90},
        {"kind": "reserve", "run_id": "r", "sig": "b", "amount": 100, "purpose": "measure"},
        {"kind": "expired", "sig": "b"},
        {"kind": "reserve", "run_id": "r", "sig": "c", "amount": 70, "purpose": "measure"},
        {"kind": "reserve", "run_id": "r", "sig": "d", "amount": 999, "purpose": "calibrate"},
    ]
    (d / "ledger.jsonl").write_text("".join(json.dumps(x) + "\n" for x in rows), encoding="utf-8")
    assert sc.committed(d) == 90 + 70


def test_print_cap_is_a_bare_integer(tmp_path, capsys):
    assert sc.main(["--k", "1", "--hours", "8", "--print", "cap", "--data", str(_data(tmp_path))]) == 0
    assert capsys.readouterr().out.strip() == "7947650"
    assert sc.main(["--k", "5"]) == 2


def test_hours_available_scales_the_cap_to_the_hours_that_are_left(tmp_path):
    """Session 2a of 2026-10-27: 3.5 h of the 6 h available (real session-1 data, copied so that
    later sessions in data/ do not change the expected numbers)."""
    run = "live-20261026T220348Z"
    d = _data(tmp_path)
    (d / "logs" / run).mkdir(parents=True)
    shutil.copy(ROOT / "data" / "logs" / run / "plan.jsonl", d / "logs" / run / "plan.jsonl")
    led = [l for l in (ROOT / "data" / "ledger.jsonl").read_text(encoding="utf-8").splitlines() if f'"run_id":"{run}"' in l]
    (d / "ledger.jsonl").write_text("".join(l + chr(10) for l in led), encoding="utf-8")
    r = sc.compute(ROOT, d, 2, 3.5, hours_available=6)
    assert r["remaining_windows"] == 265 and r["remaining_cost"] == 8_399_576
    assert r["cap"] == 5_144_741 and r["payer_target"] == 817_614
    # default unchanged: one 8 h session
    assert sc.compute(ROOT, d, 2, 3.5)["cap"] == -(-8_399_576 * 105 * 35 // (100 * 8 * 10))
    # the line per hour is the one needed for the quota (1.05 x cost / hours available)
    assert abs(r["line_per_hour"] - 8_399_576 * 1.05 / 6) <= 1
