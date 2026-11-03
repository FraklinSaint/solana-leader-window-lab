#!/usr/bin/env python3
"""POST-HOC, EXPLORATORY figure: Arm X, tau of R_PRICE by send gap, one small panel per client group.

Draws the numbers of docs/EXPLORATORY_GAP_TABLE.md (same windows, same registered per-window tau, same seeded
bootstrap: both scripts call `exploratory_gap_table.arm_x_cells` and `summarize`) as a static SVG, so the README
can show them. It adds no analysis, gives no verdict and changes none.

    python scripts/gap_figure.py            # write docs/figures/arm_x_by_gap.svg
    python scripts/gap_figure.py --check    # exit 1 if the file differs from a regeneration

Deterministic: no wall-clock values, fixed rounding, LF output, SHA-256 of the body in the last line. One series
colour (blue) on the chart surface, light and dark selected (`prefers-color-scheme` inside the SVG); the surface is
drawn as a background rectangle so the figure is legible on either page theme. The table is the text alternative.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import pathlib
import sys
from xml.sax.saxutils import escape

ROOT = pathlib.Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "figures" / "arm_x_by_gap.svg"
FOOTER = "<!-- sha256: "
GAPS = [0, 10, 25, 60, 120]  # ms; categorical positions, not to scale
THIN = 5  # fewer usable windows than this -> hollow marker

W, H = 850, 590
COLS, PITCH_X, PLOT_W, X_LEFT, X0 = 4, 205, 150, 30, 20
Y_TOP, PLOT_H, PITCH_Y = 136, 130, 205
YMAX = 1.1  # the axis shows -1..+1 with a little room so marks at +-1 are not clipped
MINUS = "−"

CSS = """\
    text { font-family: system-ui, -apple-system, "Segoe UI", Helvetica, Arial, sans-serif; font-size: 12px; }
    .bg { fill: #fcfcfb; } .ink { fill: #0b0b0b; } .ink2 { fill: #52514e; }
    .grid { stroke: #e6e5e1; stroke-width: 1; } .zero { stroke: #b9b8b3; stroke-width: 1; }
    .s { stroke: #2a78d6; fill: none; } .f { fill: #2a78d6; } .hole { fill: #fcfcfb; stroke: #2a78d6; stroke-width: 2; }
    .ring { fill: #fcfcfb; }
    .b { font-weight: 600; } .t { font-size: 17px; font-weight: 600; } .sm { font-size: 11px; }
    @media (prefers-color-scheme: dark) {
      .bg { fill: #1a1a19; } .ink { fill: #f0efec; } .ink2 { fill: #c3c2b7; }
      .grid { stroke: #2d2d2b; } .zero { stroke: #5a5a56; }
      .s { stroke: #3987e5; } .f { fill: #3987e5; } .hole { fill: #1a1a19; stroke: #3987e5; }
      .ring { fill: #1a1a19; }
    }"""


def load_table_module():
    spec = importlib.util.spec_from_file_location("exploratory_gap_table", ROOT / "scripts" / "exploratory_gap_table.py")
    mod = importlib.util.module_from_spec(spec)
    sys.modules.setdefault("exploratory_gap_table", mod)
    spec.loader.exec_module(mod)
    return mod


def num(x: float) -> str:
    return f"{x:.1f}".rstrip("0").rstrip(".") if x != int(x) else str(int(x))


def signed(x: float) -> str:
    return f"+{x:.2f}" if x >= 0 else f"{MINUS}{abs(x):.2f}"


def panels(by_gap: dict, sent_gap: dict, summarize) -> list[dict]:
    """Pooled panel first, then one per stratum (sorted). Each panel: title and {gap: summary + usable/sent}."""
    strata = sorted({k[0] for k in sent_gap})
    out = []
    pooled = {}
    for g in GAPS:
        taus = [t for (st, gg), v in sorted(by_gap.items()) if gg == g for t in v]
        sent = sum(v[0] for (st, gg), v in sent_gap.items() if gg == g)
        usable = sum(v[1] for (st, gg), v in sent_gap.items() if gg == g)
        pooled[g] = {**summarize(taus), "sent": sent, "usable": usable}
    out.append({"title": "All groups", "sub": "\u00b7 windows pooled", "cells": pooled})
    for st in strata:
        cells = {}
        for g in GAPS:
            k = (st, g)
            if k not in sent_gap:
                continue
            cells[g] = {**summarize(by_gap.get(k, [])), "sent": sent_gap[k][0], "usable": sent_gap[k][1]}
        out.append({"title": st, "sub": "", "cells": cells})
    return out


def panel_svg(idx: int, p: dict) -> list[str]:
    col, row = idx % COLS, idx // COLS
    x0 = X0 + X_LEFT + col * PITCH_X
    y0 = Y_TOP + row * PITCH_Y
    ymid = y0 + PLOT_H / 2

    def yv(v: float) -> float:
        return ymid - v * (PLOT_H / 2) / YMAX

    def xg(i: int) -> float:
        return x0 + 15 + i * (PLOT_W - 30) / (len(GAPS) - 1)

    o = [f'  <g id="panel-{idx}">']
    title = escape(p["title"]) + (f' <tspan class="ink2" font-weight="400">{escape(p["sub"])}</tspan>' if p["sub"] else "")
    o.append(f'    <text class="ink b" x="{x0}" y="{y0 - 14}">{title}</text>')
    for v, lab, cls in ((1, "+1", "grid"), (0, "0", "zero"), (-1, MINUS + "1", "grid")):
        o.append(f'    <line class="{cls}" x1="{x0}" y1="{num(yv(v))}" x2="{x0 + PLOT_W}" y2="{num(yv(v))}"/>')
        o.append(f'    <text class="ink2 sm" x="{x0 - 6}" y="{num(yv(v) + 4)}" text-anchor="end">{lab}</text>')
    # x tick labels and the usable-window row
    for i, g in enumerate(GAPS):
        o.append(f'    <text class="ink2 sm" x="{num(xg(i))}" y="{y0 + PLOT_H + 16}" text-anchor="middle">{g}</text>')
        c = p["cells"].get(g)
        n = "" if c is None else str(c["usable"])
        if n:
            o.append(f'    <text class="ink2 sm" x="{num(xg(i))}" y="{y0 + PLOT_H + 32}" text-anchor="middle">{n}</text>')
    o.append(f'    <text class="ink2 sm" x="{x0 - 6}" y="{y0 + PLOT_H + 32}" text-anchor="end">n</text>')
    # interval bars first (under the markers), then the line, then the markers with a surface ring
    pts = {}
    for i, g in enumerate(GAPS):
        c = p["cells"].get(g)
        if c and c["n"]:
            pts[i] = (xg(i), yv(c["mean"]), c)
    for i, (x, y, c) in sorted(pts.items()):
        lo, hi = c["ci"]
        o.append(f'    <line class="s" stroke-width="1.5" x1="{num(x)}" y1="{num(yv(lo))}" x2="{num(x)}" y2="{num(yv(hi))}"/>')
    run = sorted(pts)
    segs, cur = [], []
    for i in run:
        if cur and i != cur[-1] + 1:
            segs.append(cur)
            cur = []
        cur.append(i)
    if cur:
        segs.append(cur)
    for sg in segs:
        if len(sg) > 1:
            d = " ".join(f"{num(pts[i][0])},{num(pts[i][1])}" for i in sg)
            o.append(f'    <polyline class="s" stroke-width="2" stroke-linejoin="round" points="{d}"/>')
    for i, (x, y, c) in sorted(pts.items()):
        o.append(f'    <circle class="ring" cx="{num(x)}" cy="{num(y)}" r="6.5"/>')
        cls = "f" if c["usable"] >= THIN else "hole"
        o.append(f'    <circle class="{cls}" cx="{num(x)}" cy="{num(y)}" r="4.5"/>')
    o.append("  </g>")
    return o


def render(root: pathlib.Path) -> str:
    egt = load_table_module()
    wins = egt.load_windows(root)
    by_gap, sent_gap = egt.arm_x_cells(wins)
    ps = panels(by_gap, sent_gap, egt.summarize)

    pooled = ps[0]["cells"]
    desc = ("Small multiples, one panel for all groups together and one per client group. Each panel plots the mean "
            "Kendall tau between the block order and the fee order of the six transactions of Arm X, against the gap "
            "between sends (0, 10, 25, 60, 120 ms; no usable window at 120 ms). Plus one means the block follows the fee, "
            "minus one means it follows the send order. All groups pooled: "
            + "; ".join(f"{g} ms {signed(pooled[g]['mean'])} over {pooled[g]['usable']} windows" for g in GAPS if pooled[g]["n"])
            + ". The same numbers are in docs/EXPLORATORY_GAP_TABLE.md.")
    o = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" role="img" '
         'aria-labelledby="fig-title fig-desc">',
         f'  <title id="fig-title">Arm X: does the block follow the fee or the send order, by gap between sends</title>',
         f'  <desc id="fig-desc">{escape(desc)}</desc>',
         "  <style>", CSS, "  </style>",
         f'  <rect class="bg" x="0" y="0" width="{W}" height="{H}"/>',
         '  <text class="ink t" x="20" y="30">Arm X: does the block follow the fee or the send order?</text>',
         '  <text class="ink2" x="20" y="52">Mean tau of the fee rule per window, by gap between the sends. '
         f'+1: block follows the fee. {MINUS}1: block follows the send order.</text>']
    # key
    ky = 78
    o += [f'  <circle class="f" cx="28" cy="{ky - 4}" r="4.5"/>',
          f'  <text class="ink2 sm" x="38" y="{ky}">at least {THIN} usable windows</text>',
          f'  <circle class="hole" cx="190" cy="{ky - 4}" r="4.5"/>',
          f'  <text class="ink2 sm" x="200" y="{ky}">fewer than {THIN}</text>',
          f'  <line class="s" stroke-width="1.5" x1="296" y1="{ky - 10}" x2="296" y2="{ky + 2}"/>',
          f'  <text class="ink2 sm" x="306" y="{ky}">bootstrap 95% interval over windows</text>',
          f'  <text class="ink2 sm" x="20" y="{ky + 18}">x axis: gap between sends in ms (categories, not to scale). '
          'n: usable windows in that cell.</text>']
    for i, p in enumerate(ps):
        o += panel_svg(i, p)
    cy = Y_TOP + 2 * PITCH_Y - 20
    o += [f'  <text class="ink2 sm" x="20" y="{cy}">Post-hoc and exploratory: written after the confirmatory results were seen, not part of the '
          'preregistered analysis, no verdict.</text>',
          f'  <text class="ink2 sm" x="20" y="{cy + 15}">In Arm X the fee is the reverse of the send order, so the tau of the send-order '
          'rule is the same number with the opposite sign. Cells are small</text>',
          f'  <text class="ink2 sm" x="20" y="{cy + 30}">and windows of one validator are not independent; the intervals are descriptive, not tests. '
          'Table: docs/EXPLORATORY_GAP_TABLE.md.</text>',
          "</svg>"]
    body = "\n".join(o) + "\n"
    return body + FOOTER + hashlib.sha256(body.encode("utf-8")).hexdigest() + " -->\n"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", default=str(ROOT))
    ap.add_argument("--out", default=str(OUT))
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args(argv)
    text = render(pathlib.Path(a.root).resolve())
    out = pathlib.Path(a.out)
    if a.check:
        old = out.read_bytes().replace(b"\r\n", b"\n").decode("utf-8") if out.exists() else ""
        if old != text:
            print(f"{out.name} is stale or edited by hand: regenerate with python scripts/gap_figure.py", file=sys.stderr)
            return 1
        print(f"{out.name} matches its inputs")
        return 0
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
