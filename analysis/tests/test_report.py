"""slwlab.report on a SYNTHETIC repo tree (tmp dir; never data/ or docs/). Synthetic manifests
come from the simulator via tests/test_parse_analyze.synth and are labeled SIMULATION there."""

import copy
import gzip
import json
import pathlib

from slwlab import ALPENGLOW_GENESIS_SLOT, blocklist, report, sim
from test_parse_analyze import SIM_REF, synth, IDENT

LOGBOOK = """# Logbook

| UTC | Phase | Kind | Entry |
|---|---|---|---|
| 2026-10-22 15:05 | 0 | surprise | Epoch nearly over. |
| 2026-10-22 16:08 | 2 | failure | Burst timing test failed once. |
| 2026-10-22 18:05 | 2 | decision | Operator chose a provider. |
| 2026-10-23 00:30 | 1 | correction | Prereg timing corrected. |
"""
BLOCK_TIME0 = 1_793_000_000  # 2026-10-26T07:33:20Z


def write_blocklists(data: pathlib.Path, lines) -> None:
    """data/logs/<run>/blocklist.json with every slot present (the report needs the stored getBlocks list of each
    confirmatory run, as the analysis does; PREREGISTRATION §9)."""
    for run, wins in blocklist.windows_by_run(lines).items():
        blocklist.save(data, blocklist.build_record(
            run, wins, lambda m, p: 10 ** 9 if m == "getSlot" else list(range(p[0], p[1] + 1))))


def tree(tmp: pathlib.Path, lines=None, crlf=False) -> pathlib.Path:
    root = tmp / "repo"
    (root / "data").mkdir(parents=True, exist_ok=True)
    (root / "docs").mkdir(exist_ok=True)
    res = root / "analysis" / "sim" / "results"
    res.mkdir(parents=True, exist_ok=True)
    sim = copy.deepcopy(SIM_REF) | {"reps": 200, "min_ceiling": 0.3, "label": "SIMULATION"}
    (res / "sim_v2.json").write_text(json.dumps(sim), encoding="utf-8")
    nl = "\r\n" if crlf else "\n"
    (root / "docs" / "LOGBOOK.md").write_bytes(LOGBOOK.replace("\n", nl).encode())
    if lines is not None:
        (root / "data" / "manifest.jsonl").write_bytes("".join(json.dumps(l) + nl for l in lines).encode())
        write_blocklists(root / "data", lines)
    return root


def with_block_times(data_root: pathlib.Path) -> None:
    for i, p in enumerate(sorted((data_root / "raw" / "blocks").glob("*.json.gz"))):
        with gzip.open(p, "rt", encoding="utf-8") as fh:
            b = json.load(fh)
        b["blockTime"] = BLOCK_TIME0 + 86_400 * (i % 3)  # three UTC dates
        with gzip.open(p, "wt", encoding="utf-8") as fh:
            json.dump(b, fh)


def render(root):
    r = root / "analysis" / "sim" / "results"
    return report.render(root, r / "sim_v2.json", r / "alloc_v2.json")


def section(text: str, title: str) -> str:
    start = text.index(f"## {title}")
    nxt = text.find("\n## ", start + 1)
    return text[start: nxt if nxt != -1 else len(text)]


def test_no_data_yet_and_self_hash(tmp_path):
    root = tree(tmp_path)
    t = render(root)
    assert section(t, "MEASUREMENT: confirmatory results").count(report.NO_DATA) == 1
    assert report.NO_DATA in section(t, "MEASUREMENT: when (UTC)")
    assert report.NO_DATA in section(t, "EXPLORATORY: calibration")
    assert report.self_hash_ok(t)
    assert not report.self_hash_ok(t.replace("Report", "Rep0rt", 1))


def test_measurements_are_deterministic_dated_and_never_mixed_with_simulation(tmp_path):
    root = tree(tmp_path)
    lines = synth(root / "data", sim.AGAVE, windows=24)
    with_block_times(root / "data")
    (root / "data" / "manifest.jsonl").write_text("".join(json.dumps(l) + "\n" for l in lines))
    write_blocklists(root / "data", lines)
    a, b = render(root), render(root)
    assert a == b
    m = section(a, "MEASUREMENT: confirmatory results")
    assert "| Agave |" in m and "R_RATIO_AGAVE:" in m
    assert "2026-10-26, 2026-10-27, 2026-10-28" in m  # UTC dates of usable windows (block times)
    assert "SIMULATION reference" not in m and "scheduler model" not in m
    when = section(a, "MEASUREMENT: when (UTC)")
    assert "2026-10-26T07:33:20Z" in when
    # windows by UTC hour of day: every hour listed, uncovered hours marked, never filled
    assert "Windows by UTC hour of day" in when
    assert all(f"| {h:02d} |" in when for h in range(24))
    assert "not measured" in when
    s = section(a, "SIMULATION reference (not measurements)")
    assert IDENT[:8] not in s and "> SIMULATION." in s
    # order: measurement before simulation, simulation in its own section
    assert a.index("## MEASUREMENT") < a.index("## SIMULATION")


def test_crlf_and_lf_checkouts_give_the_same_bytes(tmp_path):
    lines = synth(tmp_path / "x", sim.AGAVE, windows=4)
    assert render(tree(tmp_path / "a", lines)) == render(tree(tmp_path / "b", lines, crlf=True))


def test_nonmainnet_and_pre_alpenglow_are_excluded(tmp_path):
    lines = synth(tmp_path / "x", sim.AGAVE, windows=3)
    bad = copy.deepcopy(lines[:2])
    bad[0]["tx"]["cluster"] = "devnet"
    bad[1]["tx"]["first_slot"] = ALPENGLOW_GENESIS_SLOT - 10
    root = tree(tmp_path, lines + bad)
    t = render(root)
    assert "manifest: not_mainnet 1, pre_alpenglow 1" in t
    # data/nonmainnet is never read, whatever it contains
    nm = root / "data" / "nonmainnet" / "devnet"
    nm.mkdir(parents=True)
    (nm / "manifest.jsonl").write_text("".join(json.dumps(l) + "\n" for l in lines))
    assert render(root) == t


def test_what_went_wrong_from_logbook(tmp_path):
    w = section(render(tree(tmp_path)), "What went wrong")
    assert "2026-10-22T16:08:00Z | 2 | failure" in w
    assert "Operator chose" not in w  # decisions are not failures
    assert "2026-10-22T21:30:00Z † | 1 | correction" in w  # local-time row converted, marked


def test_check_mode_detects_stale_or_edited_report(tmp_path):
    root = tree(tmp_path)
    assert report.main(["--root", str(root)]) == 0
    assert report.main(["--root", str(root), "--check"]) == 0
    p = root / "docs" / "REPORT.md"
    p.write_text(p.read_text(encoding="utf-8").replace("NO DATA YET", "great results"), encoding="utf-8")
    assert report.main(["--root", str(root), "--check"]) == 1
    report.main(["--root", str(root)])
    with open(root / "docs" / "LOGBOOK.md", "a", encoding="utf-8") as fh:
        fh.write("| 2026-10-22 22:00 | 4 | failure | New failure. |\n")
    assert report.main(["--root", str(root), "--check"]) == 1  # input changed -> stale


def test_a_confirmatory_run_without_its_slot_list_stops_the_report(tmp_path):
    import pytest
    lines = synth(tmp_path / "x", sim.AGAVE, windows=3)
    root = tree(tmp_path, lines)
    for p in (root / "data" / "logs").rglob("blocklist.json"):
        p.unlink()
    with pytest.raises(SystemExit, match="no stored getBlocks list"):
        render(root)


def test_a_window_with_a_skipped_slot_is_not_usable_in_the_report_either(tmp_path):
    lines = synth(tmp_path / "x", sim.AGAVE, windows=4)
    root = tree(tmp_path, lines)
    (p,) = (root / "data" / "logs").rglob("blocklist.json")
    rec = json.loads(p.read_text(encoding="utf-8"))
    wid = sorted(rec["windows"])[0]
    rec["windows"][wid]["slots_with_blocks"] = rec["windows"][wid]["slots_with_blocks"][1:]
    p.write_text(json.dumps(rec), encoding="utf-8")
    t = render(root)
    when = section(t, "MEASUREMENT: when (UTC)")
    assert f"| {wid} |" in when and "skipped_slot" in when.split(f"| {wid} |", 1)[1].split("\n", 1)[0]
