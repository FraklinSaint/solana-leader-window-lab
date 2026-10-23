"""slwlab.sessions: planner-equivalent grouping, session caps, spacing, loss, power lookup."""

import gzip
import json

import pytest

from slwlab import sessions as ss

NS = 10**9
H = 3_600 * NS


def test_windows_merge_back_to_back_like_the_planner():
    e = {"first_slot": 100, "leader": ["A"] * 8 + ["B"] * 4 + [None] * 4 + ["A"] * 4}
    assert ss.windows(e) == [(100, 8, "A"), (108, 4, "B"), (116, 4, "A")]


def test_pick_arm_largest_remaining_share_c_first_on_ties():
    assert ss.pick_arm({"P": 4, "C": 2}, {}) == "C"
    assert ss.pick_arm({"P": 4, "C": 2}, {"C": 1}) == "P"
    assert ss.pick_arm({"P": 1}, {"P": 1}) is None


def test_sessions_split_quota_and_respect_spacing():
    # A every 10 s, B 2 s after each A (always too close)
    stream = sorted([(k * 10 * NS, "A") for k in range(1000)] + [(k * 10 * NS + 2 * NS, "B") for k in range(1000)])
    r = ss.run_sessions(stream, {"A": ["A"], "B": ["B"]}, {"A": {"P": 6}, "B": {"P": 2}},
                        [(0, 20 * NS), (100 * NS, 10_000 * NS)], 0.0, 1)
    # session 1 cap ceil(6/2)=3 for A: A at 0, 10 s, (20 s is outside); B at 2 s too close to A
    assert r["per_session"][0] == {"A": 2, "B": 0}
    assert r["done"] == {"A": {"P": 6}, "B": {"P": 2}}
    assert r["spacing_lost"] >= 1


def test_loss_thins_windows_without_using_quota():
    stream = [(k * 10 * NS, "A") for k in range(100)]
    full = ss.run_sessions(stream, {"A": ["A"]}, {"A": {"P": 100}}, [(0, 1000 * NS)], 0.0, 1)
    lossy = ss.run_sessions(stream, {"A": ["A"]}, {"A": {"P": 100}}, [(0, 1000 * NS)], 0.5, 1)
    assert full["done"]["A"]["P"] == 100
    assert 30 < lossy["done"]["A"]["P"] < 70


def test_power_is_a_floor_on_the_grid_never_interpolated():
    curves = {"agave/C": {"power": {"10": {"half": 0.7, "exact": 1.0}, "15": {"half": 0.9, "exact": 1.0}}}}
    assert ss.power_at(curves, "agave", "C", 14.9) == {"w": 10, "half": 0.7, "exact": 1.0, "below_grid": False}
    assert ss.power_at(curves, "agave", "C", 9.0)["below_grid"]
    assert ss.power_at(curves, "fd2", "C", 50)["w"] is None


def _data(tmp, schedule=True):
    a, b = 1_790_000_000 * NS, 212_000_000
    d = tmp / "logs" / "dryrun-20261022T000000Z"
    d.mkdir(parents=True)
    (d / "run.json").write_text(json.dumps({"cluster": "mainnet"}))
    (d / "slots.jsonl").write_text("".join(json.dumps({"slot": s, "t_ns": a + b * s}) + "\n" for s in range(0, 300)))
    snaps = tmp / "snapshots"
    snaps.mkdir(parents=True)
    (snaps / "epoch_info_20261023T000000Z.json").write_text(json.dumps({
        "cluster": "mainnet", "absolute_slot": 400_000, "request_unix_ns": str(a + b * 400_000),
        "response_unix_ns": str(a + b * 400_000)}))
    if schedule:
        n = 432_000  # one epoch, ~25.4 h: A leads 1 window in 10, B 1 in 40, C 1 in 200
        sched = {"A": [], "B": [], "C": []}
        for w in range(n // 4):
            who = "A" if w % 10 == 0 else "B" if w % 40 == 5 else "C" if w % 200 == 7 else None
            if who:
                sched[who] += range(4 * w, 4 * w + 4)
        with gzip.open(snaps / "leader_schedule_epoch1065.json.gz", "wt", encoding="utf-8") as f:
            json.dump({"cluster": "mainnet", "epoch": 1065, "first_slot": 0, "slots_in_epoch": n, "schedule": sched}, f)
    sel = tmp / "sel.json"
    sel.write_text(json.dumps({
        "picked": [{"identity": "A", "stratum": "JitoAgave"}, {"identity": "B", "stratum": "Agave", "version": "4.3.2"}],
        "candidates": [{"identity": "C", "stratum": "Agave", "version": "4.3.2", "confidence": "medium"}]}))
    alloc = tmp / "alloc.json"
    curve = {"power": {str(w): {"half": min(1, w / 40), "exact": 1.0} for w in (10, 15, 20, 30, 40, 80)}}
    alloc.write_text(json.dumps({
        "allocation": {"plan_quota": {"JitoAgave": {"P": 20, "C": 10}, "Agave": {"P": 20, "C": 10},
                                      "BamFiredancer": {"P": 5}},
                       "usable_rate": 0.9, "spend_per_window_lamports": {"P": 100.0, "C": 200.0},
                       "confirmatory_budget_lamports": 10**6, "confirmatory_total_lamports": 10**5},
        "curves": {"agave/P": curve, "agave/C": curve}}))
    return sel, alloc


def test_run_end_to_end_both_options(tmp_path):
    sel, alloc = _data(tmp_path)
    r = ss.run(tmp_path, sel, alloc, loss=0.0, calib_h=0.0)
    assert any("BamFiredancer" in n for n in r["notes"])
    v = {x["identity"]: x for x in r["validators"]}
    assert v["B"]["windows_per_hour"] == pytest.approx(2700 / r["inputs"]["snapshot_hours"], rel=1e-6)
    assert v["B"]["hours_validator"] == pytest.approx(30 / v["B"]["windows_per_hour"])
    s = r["scenarios"][0]
    assert s["B"]["pooled_validators"] == ["B", "C"]
    pooled = [c for c in s["B"]["cells"] if c["owner"] == ss.POOL]
    assert {c["arm"]: c["quota"] for c in pooled} == {"P": 20, "C": 10}   # same total as A's one validator
    assert all(c["cost"] == c["attempted"] * (100 if c["arm"] == "P" else 200) for c in s["A"]["cells"])
    md = ss.markdown(r)
    assert "PLANNING" in md and "Option B" in md and "not comparable" in md


def test_no_schedule_is_an_error_not_a_guess(tmp_path):
    sel, alloc = _data(tmp_path, schedule=False)
    with pytest.raises(SystemExit, match="no leader schedule"):
        ss.run(tmp_path, sel, alloc)
