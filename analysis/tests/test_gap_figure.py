"""scripts/gap_figure.py: the post-hoc figure is generated from the same cells as the table, deterministic and valid SVG."""
import importlib.util
import pathlib
import xml.etree.ElementTree as ET

ROOT = pathlib.Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("gap_figure", ROOT / "scripts" / "gap_figure.py")
gf = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gf)


def fake_summarize(taus):
    if not taus:
        return {"n": 0, "mean": None, "ci": (None, None), "share_pos": None, "share_neg": None}
    m = sum(taus) / len(taus)
    return {"n": len(taus), "mean": m, "ci": (m - 0.1, m + 0.1), "share_pos": 0.0, "share_neg": 0.0}


def test_panels_pool_all_strata_first_and_keep_empty_cells_empty():
    by_gap = {("A", 0): [1.0, 0.5], ("B", 0): [-1.0], ("A", 10): [-0.5]}
    sent = {("A", 0): [3, 2], ("B", 0): [2, 1], ("A", 10): [2, 1], ("A", 120): [2, 0]}
    ps = gf.panels(by_gap, sent, fake_summarize)
    assert [p["title"] for p in ps] == ["All groups", "A", "B"]
    pooled = ps[0]["cells"]
    assert pooled[0]["n"] == 3 and pooled[0]["usable"] == 3 and pooled[0]["sent"] == 5
    assert ps[1]["cells"][120]["n"] == 0 and ps[1]["cells"][120]["usable"] == 0
    assert 25 not in ps[1]["cells"]


def test_the_svg_is_well_formed_and_carries_a_text_alternative():
    p = {"title": "A<&>", "sub": "", "cells": {0: {**fake_summarize([0.4]), "sent": 3, "usable": 1}}}
    svg = "\n".join(gf.panel_svg(0, p))
    root = ET.fromstring('<svg xmlns="http://www.w3.org/2000/svg">' + svg + "</svg>")
    assert root.find(".//{http://www.w3.org/2000/svg}circle[@class='hole']") is not None  # 1 usable window < THIN
    assert "A&lt;&amp;&gt;" in svg


def test_the_committed_figure_matches_its_inputs():
    out = ROOT / "docs" / "figures" / "arm_x_by_gap.svg"
    if not ((ROOT / "data" / "manifest.jsonl").is_file() and out.is_file()):
        return
    assert gf.main(["--check"]) == 0
    root = ET.fromstring(out.read_text(encoding="utf-8").split("<!-- sha256")[0])
    assert root.find("{http://www.w3.org/2000/svg}title") is not None
    assert root.find("{http://www.w3.org/2000/svg}desc") is not None
