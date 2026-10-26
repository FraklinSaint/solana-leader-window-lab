"""analyze.main refuses confirmatory data that has no stored getBlocks list (PREREGISTRATION §9)."""
import json

import pytest

from slwlab import analyze, blocklist, sim

from test_parse_analyze import SIM_REF, synth


def _write(tmp_path, lines):
    m = tmp_path / "manifest.jsonl"
    m.write_text("".join(json.dumps(l) + "\n" for l in lines), encoding="utf-8")
    s = tmp_path / "sim.json"
    s.write_text(json.dumps(SIM_REF), encoding="utf-8")
    return ["--manifest", str(m), "--sim", str(s), "--out", str(tmp_path / "out" / "res.json")]


def test_confirmatory_run_without_a_list_is_refused_with_the_fix(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=6)  # purpose "measure"
    args = _write(tmp_path, lines)
    with pytest.raises(SystemExit, match="--data"):
        analyze.main(args)
    with pytest.raises(SystemExit) as e:
        analyze.main(args + ["--data", str(tmp_path)])
    msg = str(e.value)
    assert "live-20261101T000000Z" in msg and "blocklist fetch" in msg and "S8" in msg
    assert not (tmp_path / "out" / "res.json").exists()


def test_with_a_list_it_runs_and_reports_skipped_windows(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=6)
    wins = blocklist.windows_by_run(lines)
    rec = blocklist.build_record("live-20261101T000000Z", wins["live-20261101T000000Z"],
                                 lambda m, p: 10 ** 9 if m == "getSlot" else list(range(p[0], p[1] + 1)))
    blocklist.save(tmp_path, rec)
    analyze.main(_write(tmp_path, lines) + ["--data", str(tmp_path)])
    out = json.loads((tmp_path / "out" / "res.json").read_text(encoding="utf-8"))
    (r,) = out["skipped_slots_by_validator"].values()
    assert r["windows"] == 6 and r["windows_with_skipped_slot"] == 0


def test_exploratory_manifest_needs_no_list_unless_asked(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=6)
    for l in lines:
        l["tx"]["purpose"] = "calibrate"
    analyze.main(_write(tmp_path, lines))  # no --data: the old behaviour
    with pytest.raises(SystemExit, match="--data"):
        analyze.main(_write(tmp_path, lines) + ["--require-blocklists"])
