"""scripts/path_hourly_table.py (PREREGISTRATION 6d per-hour table): synthetic session directories only."""
import importlib.util
import json
import pathlib

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("path_hourly_table", ROOT / "scripts" / "path_hourly_table.py")
pht = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pht)


def test_nearest_rank_matches_netmon_rs():
    v = [float(x) for x in range(1, 11)]  # 1..10
    assert pht.nearest_rank(v, 0.5) == 5.0      # ceil(5.0) = 5th
    assert pht.nearest_rank(v, 0.05) == 1.0     # ceil(0.5) = 1st
    assert pht.nearest_rank(v, 0.95) == 10.0    # ceil(9.5) = 10th
    assert pht.nearest_rank([7.0], 0.95) == 7.0
    assert pht.nearest_rank([], 0.5) is None


def _wifi(t, q):
    return {"network_type": "wifi", "t": t, "unix": 0, "wifi": None if q is None else {"signal_quality": q}}


def test_signal_by_hour_buckets_and_skips_unreadable_links():
    lines = [_wifi("2026-11-02T12:38:39Z", 98), _wifi("2026-11-02T12:39:09Z", 84), _wifi("2026-11-02T12:59:59Z", None),
             _wifi("2026-11-02T13:00:00Z", 100)]
    s = pht.signal_by_hour(lines)
    assert set(s) == {"2026-11-02T12:00:00Z", "2026-11-02T13:00:00Z"}
    assert s["2026-11-02T12:00:00Z"] == {"n": 2, "min": 84.0, "p5": 84.0, "median": 84.0, "p95": 98.0, "max": 98.0}
    assert s["2026-11-02T13:00:00Z"]["n"] == 1


def _session(tmp_path, rtt_hours, cov_hours, wifi):
    d = tmp_path / "sessions" / "S1"
    d.mkdir(parents=True)
    (d / "session.json").write_text(json.dumps({"mode": "measure"}), encoding="utf-8")
    (d / "network.json").write_text(json.dumps({"rtt_by_hour": [
        {"hour_utc": h, "n": 10, "n_err": 0, "loss_share": 0.0, "p50_ms": 50.0, "p95_ms": 90.0, "p99_ms": 99.0,
         "jitter_ms": 12.5, "sigma_mad_ms": 4.0, "max_ms": 120.0} for h in rtt_hours]}), encoding="utf-8")
    (d / "coverage.json").write_text(json.dumps({"hours": [
        {"hour_utc": h, "covered_secs": 3500, "flag": "OK", "windows_executed": 7, "windows_skipped": 0,
         "clock_err": {"median_abs_ms": 80.0, "p90_abs_ms": 150.0, "p95_abs_ms": 200.0, "max_abs_ms": 300.0}}
        for h in cov_hours]}), encoding="utf-8")
    (d / "wifi.jsonl").write_text("\n".join(json.dumps(w) for w in wifi) + "\n", encoding="utf-8")
    return d


def test_join_keeps_missing_hours_as_none(tmp_path):
    h12, h13, h14 = ("2026-11-02T12:00:00Z", "2026-11-02T13:00:00Z", "2026-11-02T14:00:00Z")
    d = _session(tmp_path, [h12, h13], [h12, h14], [_wifi("2026-11-02T13:10:00Z", 90)])
    rows = {r["hour_utc"]: r for r in pht.session_rows(d)}
    assert sorted(rows) == [h12, h13, h14]
    assert rows[h12]["rtt"]["jitter_ms"] == 12.5 and rows[h12]["clock_err"]["median_abs_ms"] == 80.0
    assert rows[h12]["signal_quality"] is None                      # no wifi sample that hour: not filled
    assert rows[h13]["clock_err"] is None and rows[h13]["covered_secs"] is None
    assert rows[h13]["signal_quality"]["median"] == 90.0
    assert rows[h14]["rtt"] is None and rows[h14]["flag"] == "OK"


def test_measure_sessions_filter_and_cli_write_once(tmp_path, capsys):
    h = "2026-11-02T12:00:00Z"
    _session(tmp_path, [h], [h], [_wifi("2026-11-02T12:01:00Z", 97)])
    other = tmp_path / "sessions" / "S0"
    other.mkdir()
    (other / "session.json").write_text(json.dumps({"mode": "dryrun"}), encoding="utf-8")
    assert pht.measure_sessions(tmp_path) == ["S1"]
    out = tmp_path / "o" / "t.json"
    assert pht.main(["--data", str(tmp_path), "--out-json", str(out)]) == 0
    assert json.loads(out.read_text(encoding="utf-8"))["sessions"]["S1"][0]["hour_utc"] == h
    assert "session S1" in capsys.readouterr().out
    with pytest.raises(SystemExit):
        pht.main(["--data", str(tmp_path), "--out-json", str(out)])
