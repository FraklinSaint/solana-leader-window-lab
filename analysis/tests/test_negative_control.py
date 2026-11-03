"""scripts/negative_control.py on SYNTHETIC manifests only (never the real data)."""
import importlib.util
import json
import pathlib

from slwlab import analyze, parse, sim

from test_parse_analyze import SIM_REF, synth
from test_pooled_analysis import _lines

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("negative_control", ROOT / "scripts" / "negative_control.py")
nc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(nc)


def test_shuffle_permutes_only_usable_windows_keeps_positions_and_is_seeded(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=12, seed=3)
    base = parse.windows_from_manifest(lines)
    a, b, c = (parse.windows_from_manifest(lines) for _ in range(3))
    nc.shuffle_windows(a, 7)
    nc.shuffle_windows(b, 7)
    nc.shuffle_windows(c, 8)
    moved = 0
    for w0, wa, wb, wc in zip(base, a, b, c):
        assert wa.positions == w0.positions
        assert sorted(t.idx for t in wa.landed) == sorted(t.idx for t in w0.landed)
        assert [t.idx for t in wa.landed] == [t.idx for t in wb.landed]   # same seed, same permutation
        # arrival times travel with their tx
        assert {t.idx: s for t, s in zip(wa.landed, wa.send_ns)} == {t.idx: s for t, s in zip(w0.landed, w0.send_ns)}
        moved += [t.idx for t in wa.landed] != [t.idx for t in w0.landed]
    assert moved > 0 and any([t.idx for t in x.landed] != [t.idx for t in y.landed] for x, y in zip(a, c))
    assert all(wa.landed == w0.landed for w0, wa in zip(base, a) if not w0.usable)


def test_wrapper_is_restored_and_analysis_code_is_untouched(tmp_path):
    original = parse.windows_from_manifest
    with nc.shuffled_windows(1):
        assert parse.windows_from_manifest is not original
    assert parse.windows_from_manifest is original
    try:
        with nc.shuffled_windows(1):
            raise RuntimeError("x")
    except RuntimeError:
        pass
    assert parse.windows_from_manifest is original


def test_shuffling_removes_a_real_rule_signal(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=40, seed=5)
    real = analyze.analyze(lines, SIM_REF)["cells"][0]
    assert nc.has_hit(real["rules"])
    hits = 0
    for r in range(8):
        with nc.shuffled_windows(nc.BASE_SEED + r):
            cell = analyze.analyze(lines, SIM_REF)["cells"][0]
        hits += nc.has_hit(cell["rules"])
    assert hits <= 2  # the Holm family false-positive rate is about 0.05 per shuffle


def test_run_shuffles_counts_families_and_passes_on_a_null_like_dataset(tmp_path):
    lines = _lines(tmp_path, sim.AGAVE, sim.AGAVE)
    res = nc.run_shuffles(lines, SIM_REF, None, False, n=4, pooled_stratum="Agave", ref_reps=10)
    fams = res["hits_per_family"]
    assert any(k.startswith("pooled Agave arm P") for k in fams) and sum(k.startswith("validator ") for k in fams) == 2
    assert res["shuffles"] == 4 and all(0 <= v <= 4 for v in fams.values())
    assert res["pass"] == (not res["families_over_limit"]) and res["pooled_dropped_for_time"] is False
    assert len(res["seconds_per_shuffle"]) == 4


def test_pooled_cells_are_dropped_for_all_shuffles_when_the_first_is_too_slow(tmp_path):
    lines = _lines(tmp_path, sim.AGAVE, sim.AGAVE)
    res = nc.run_shuffles(lines, SIM_REF, None, False, n=3, pooled_stratum="Agave", ref_reps=10, time_limit_s=-1.0)
    assert res["pooled_dropped_for_time"] is True
    assert not any(k.startswith("pooled") for k in res["hits_per_family"])


def test_limit_is_more_than_three_of_twenty():
    assert nc.SHUFFLES == 20 and nc.MAX_HITS == 3 and nc.BASE_SEED == 20261022
    over = {f: k for f, k in {"a": 3, "b": 4}.items() if k > nc.MAX_HITS}
    assert over == {"b": 4}


def test_cli_writes_once_on_synthetic_data(tmp_path):
    lines = synth(tmp_path, sim.AGAVE, windows=10, seed=2)
    for l in lines:
        l["tx"]["purpose"] = "calibrate"  # exploratory: no getBlocks list needed in this synthetic test
    (tmp_path / "m.jsonl").write_text("".join(json.dumps(l) + "\n" for l in lines), encoding="utf-8")
    (tmp_path / "s.json").write_text(json.dumps(SIM_REF), encoding="utf-8")
    argv = ["--manifest", str(tmp_path / "m.jsonl"), "--sim", str(tmp_path / "s.json"), "--out",
            str(tmp_path / "o" / "n.json"), "--shuffles", "2", "--pooled", "none"]
    nc.main(argv)
    out = json.loads((tmp_path / "o" / "n.json").read_text(encoding="utf-8"))
    assert out["shuffles"] == 2 and out["pooled_stratum"] is None
    try:
        nc.main(argv)
        raise AssertionError("must refuse to overwrite")
    except SystemExit:
        pass
