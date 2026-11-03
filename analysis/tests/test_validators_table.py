"""scripts/validators_table.py: per-validator identification and results, generated from committed files."""
import importlib.util
import pathlib

ROOT = pathlib.Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("validators_table", ROOT / "scripts" / "validators_table.py")
vt = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(vt)


def test_cell_formats_verdict_and_tau():
    c = {"rules": {"R_PRICE": {"verdict": "STRICT_RULE_LOW_SEPARATION", "mean_tau": 0.8123}}}
    assert vt.cell(c, "R_PRICE") == "STRICT (low separation), +0.81"
    assert vt.cell(c, "R_FIFO") == "–" and vt.cell(None, "R_PRICE") == "–"


def test_the_committed_file_matches_its_inputs():
    out = ROOT / "docs" / "VALIDATORS.md"
    if not ((ROOT / vt.SELECTION).is_file() and (ROOT / vt.RESULTS).is_file() and out.is_file()):
        return
    assert vt.main(["--check"]) == 0


def test_every_validator_of_the_manifest_is_listed():
    import json
    if not ((ROOT / vt.SELECTION).is_file() and (ROOT / "data" / "manifest.jsonl").is_file()):
        return
    ids = {json.loads(l)["tx"]["identity"] for l in (ROOT / "data" / "manifest.jsonl").read_text(encoding="utf-8").splitlines() if l}
    sel = {p["identity"] for p in json.loads((ROOT / vt.SELECTION).read_text(encoding="utf-8"))["picked"]}
    assert ids == sel
