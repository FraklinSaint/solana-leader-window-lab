"""scripts/exploratory_gap_table.py: the post-hoc split of Arm X by send gap (EXPLORATORY, no verdict)."""
import importlib.util
import math
import pathlib

ROOT = pathlib.Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("gap_table", ROOT / "scripts" / "exploratory_gap_table.py")
gt = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gt)


def test_summarize_counts_means_and_shares():
    s = gt.summarize([1.0, 0.5, -0.5, -1.0, float("nan")])
    assert s["n"] == 4 and math.isclose(s["mean"], 0.0, abs_tol=1e-12)
    assert s["share_pos"] == 0.5 and s["share_neg"] == 0.5
    assert s["ci"][0] <= s["mean"] <= s["ci"][1]


def test_summarize_is_deterministic_and_handles_empty():
    assert gt.summarize([0.2, 0.4, 0.9]) == gt.summarize([0.2, 0.4, 0.9])
    e = gt.summarize([])
    assert e["n"] == 0 and e["mean"] is None


def test_the_committed_file_matches_the_data():
    out = ROOT / "docs" / "EXPLORATORY_GAP_TABLE.md"
    if not ((ROOT / "data" / "manifest.jsonl").is_file() and out.is_file()):
        return
    assert gt.main(["--check"]) == 0


def test_the_file_says_it_is_post_hoc_and_gives_no_verdict():
    out = ROOT / "docs" / "EXPLORATORY_GAP_TABLE.md"
    if not out.is_file():
        return
    t = out.read_text(encoding="utf-8")
    assert "EXPLORATORY, POST-HOC" in t
    assert "STRICT_RULE" not in t and "SOFT_RULE" not in t and "NO_RULE" not in t
