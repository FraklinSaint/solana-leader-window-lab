"""scripts/filter_manifest_session.py: byte-exact filtering of a manifest by session. Synthetic data only."""
import importlib.util
import json
import pathlib

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("filter_manifest_session", ROOT / "scripts" / "filter_manifest_session.py")
fm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fm)


def _line(run, i, nl=b"\n"):
    return json.dumps({"tx": {"run_id": run, "idx": i}, "outcome": {"status": "landed"}}).encode() + nl


def _setup(tmp_path):
    (tmp_path / "data" / "sessions" / "S2").mkdir(parents=True)
    (tmp_path / "data" / "sessions" / "S2" / "coverage.json").write_text(
        json.dumps({"runs": ["data\\logs\\live-B"]}), encoding="utf-8")
    m = tmp_path / "manifest.jsonl"
    m.write_bytes(_line("live-A", 0) + _line("live-B", 1) + _line("live-A", 2, b"\r\n") + _line("live-B", 3))
    return m


def test_run_ids_from_windows_style_paths(tmp_path):
    _setup(tmp_path)
    assert fm.run_ids_of_session(tmp_path / "data", "S2") == {"live-B"}


def test_filter_drops_only_that_run_and_keeps_bytes(tmp_path):
    m = _setup(tmp_path)
    out = tmp_path / "out" / "m.jsonl"
    assert fm.main(["--manifest", str(m), "--data", str(tmp_path / "data"), "--session", "S2", "--out", str(out)]) == 0
    assert out.read_bytes() == _line("live-A", 0) + _line("live-A", 2, b"\r\n")


def test_refuses_overwrite_output_in_data_and_unknown_run(tmp_path):
    m = _setup(tmp_path)
    data = str(tmp_path / "data")
    out = tmp_path / "m.jsonl"
    out.write_bytes(b"x")
    with pytest.raises(SystemExit):
        fm.main(["--manifest", str(m), "--data", data, "--session", "S2", "--out", str(out)])
    with pytest.raises(SystemExit):
        fm.main(["--manifest", str(m), "--data", data, "--session", "S2", "--out", str(tmp_path / "data" / "m.jsonl")])
    m.write_bytes(_line("live-A", 0))
    with pytest.raises(SystemExit):  # session's run is not in the manifest
        fm.main(["--manifest", str(m), "--data", data, "--session", "S2", "--out", str(tmp_path / "new.jsonl")])
