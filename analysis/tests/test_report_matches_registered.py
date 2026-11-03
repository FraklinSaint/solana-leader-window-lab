"""The report's measurement table is the registered analysis, not a second implementation.

Runs the real analysis on the committed data (about 1.5 minutes) and compares it, number by number, with the
committed `analysis/results/confirmatory_2026-11-03/results.json` that the registered command produced.
Skipped when the data or the committed results are not in the tree (a partial checkout).
"""
import importlib.util
import json
import pathlib

from slwlab import parse, report

ROOT = pathlib.Path(__file__).resolve().parents[2]
RESULTS = ROOT / "analysis" / "results" / "confirmatory_2026-11-03" / "results.json"
MANIFEST = ROOT / "data" / "manifest.jsonl"
SIM = ROOT / "analysis" / "sim" / "results" / "sim_v2.json"

_spec = importlib.util.spec_from_file_location("reproduce", ROOT / "scripts" / "reproduce.py")
rep = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(rep)


def test_report_table_is_the_committed_results():
    if not (RESULTS.is_file() and MANIFEST.is_file() and SIM.is_file()):
        return  # nothing to compare with in this checkout
    lines, dropped = report.split_scope(parse.read_jsonl(MANIFEST))
    assert not dropped, f"lines dropped by the report's scope filter: {dict(dropped)}"
    assert all(l["tx"].get("purpose") == "measure" for l in lines)  # the report keeps only these
    sim = json.loads(SIM.read_text(encoding="utf-8"))
    res = report.registered_analysis(lines, sim, ROOT / "data")
    ref = json.loads(RESULTS.read_text(encoding="utf-8"))
    acc = rep.compare(ref, json.loads(json.dumps(res)))  # via JSON, as the CLI writes it
    assert acc["diffs"] == [], acc["diffs"][:5]
    assert acc["numbers"] > 1000
