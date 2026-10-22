"""slwlab.report on a SYNTHETIC repo tree (tmp dir; never data/ or docs/). Synthetic manifests
come from the simulator via tests/test_parse_analyze.synth and are labeled SIMULATION there."""

import copy
import gzip
import json
import pathlib

from slwlab import ALPENGLOW_GENESIS_SLOT, report, sim
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


def tree(tmp: pathlib.Path, lines=None, crlf=False) -> pathlib.Path:
    root = tmp / "repo"
    (root / "data").mkdir(parents=True, exist_ok=True)
    (root / "docs").mkdir(exist_ok=True)
    res = root / "analysis" / "sim" / "results"
    res.mkdir(parents=True, exist_ok=True)
    sim = copy.deepcopy(SIM_REF) | {"reps": 200, "min_ceiling": 0.3, "label": "SIMULATION"}
    (res / "sim_v1.json").write_text(json.dumps(sim), encoding="utf-8")
    nl = "\r\n" if crlf else "\n"
    (root / "docs" / "LOGBOOK.md").write_bytes(LOGBOOK.replace("\n", nl).encode())
    if lines is not None:
        (root / "data" / "manifest.jsonl").write_bytes("".join(json.dumps(l) + nl for l in lines).encode())
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
    return report.render(root, r / "sim_v1.json", r / "alloc_v1.json")


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
    a, b = render(root), render(root)
    assert a == b
    m = section(a, "MEASUREMENT: confirmatory results")
    assert "| Agave |" in m and "R_RATIO_AGAVE:" in m
    assert "2026-10-26, 2026-10-27, 2026-10-28" in m  # UTC dates of usable windows (block times)
    assert "SIMULATION reference" not in m and "scheduler model" not in m
    when = section(a, "MEASUREMENT: when (UTC)")
    assert "2026-10-26T07:33:20Z" in when
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
