"""docs/REPORT.md from committed files only (`docker compose run --rm analyze`).

    python -m slwlab.report            # write docs/REPORT.md
    python -m slwlab.report --check    # regenerate in memory; exit 1 if docs/REPORT.md differs

Deterministic: the same inputs give the same bytes on any OS. There are no wall-clock
timestamps, the analysis seed is fixed, every table is sorted, and input hashes are taken over
LF-normalized content (a Windows checkout has CRLF). The last line holds the SHA-256 of
everything above it.

Scope: only `data/manifest.jsonl` (MEASUREMENT) and `data/exploratory/calibration_manifest.jsonl`
(EXPLORATORY) are read. `data/nonmainnet/` is never opened. Lines from another cluster, or
with a slot before the Alpenglow guard, are dropped and counted. SIMULATION values (ceilings,
thresholds, power) come from `analysis/sim/results/` and are shown in their own section.
Measured tables show only the verdicts and normalized tau that depend on them.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import pathlib
import sys
from collections import Counter, defaultdict

from . import ALPENGLOW_GENESIS_SLOT, analyze, blocklist, parse, rules
from .utc import CORRECTED_LOGBOOK_ROWS, LOCAL_OFFSET_OF_CORRECTED_ROWS_H

ROOT = pathlib.Path(__file__).resolve().parents[2]
SEED = 20261022
FOOTER = "<!-- report-sha256: "
WRONG_KINDS = ("failure", "surprise", "rate limit", "env", "limitation", "correction", "missed window")
NO_DATA = "**NO DATA YET.**"


# ---------- inputs ----------

def norm_bytes(p: pathlib.Path) -> bytes:
    return p.read_bytes().replace(b"\r\n", b"\n")


def sha(p: pathlib.Path) -> str | None:
    return hashlib.sha256(norm_bytes(p)).hexdigest() if p.exists() else None


def in_scope(line: dict) -> str | None:
    """None if the line may be analyzed, else the reason it is dropped."""
    t, o = line["tx"], line["outcome"]
    if t.get("cluster") != "mainnet":
        return "not_mainnet"
    if t["first_slot"] < ALPENGLOW_GENESIS_SLOT or (o.get("slot") is not None and o["slot"] < ALPENGLOW_GENESIS_SLOT):
        return "pre_alpenglow"
    return None


def split_scope(lines: list[dict]) -> tuple[list[dict], Counter]:
    keep, dropped = [], Counter()
    for l in lines:
        r = in_scope(l)
        if r is None:
            keep.append(l)
        else:
            dropped[r] += 1
    return keep, dropped


def utc_z(unix_s: int | None) -> str:
    if unix_s is None:
        return "–"
    return dt.datetime.fromtimestamp(unix_s, tz=dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def block_time(data_root: pathlib.Path, slot: int | None, cache: dict) -> int | None:
    if slot is None:
        return None
    if slot not in cache:
        p = data_root / "raw" / "blocks" / f"{slot}.json.gz"
        cache[slot] = parse.load_block(data_root, slot).get("blockTime") if p.exists() else None
    return cache[slot]


def run_start_utc(run_id: str) -> str:
    """`live-20261101T093000Z` -> 2026-11-01T09:30:00Z (run ids are UTC compact stamps)."""
    s = run_id.rsplit("-", 1)[-1]
    try:
        return dt.datetime.strptime(s, "%Y%m%dT%H%M%SZ").replace(tzinfo=dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        return "–"


# ---------- logbook ----------

def logbook_rows(path: pathlib.Path) -> list[dict]:
    rows = []
    if not path.exists():
        return rows
    for line in norm_bytes(path).decode("utf-8").splitlines():
        if not line.startswith("| 20"):
            continue
        ts, phase, kind, entry = (c.strip() for c in line.strip().strip("|").split(" | ", 3))
        t = dt.datetime.strptime(ts, "%Y-%m-%d %H:%M").replace(tzinfo=dt.timezone.utc)
        corrected = ts in CORRECTED_LOGBOOK_ROWS
        if corrected:
            t -= dt.timedelta(hours=LOCAL_OFFSET_OF_CORRECTED_ROWS_H)
        rows.append({"utc": t.strftime("%Y-%m-%dT%H:%M:00Z"), "corrected": corrected, "phase": phase,
                     "kind": kind, "entry": entry.rstrip(" |")})
    return rows


# ---------- rendering ----------

def f(x, nd=2) -> str:
    if x is None:
        return "–"
    if isinstance(x, float):
        if x != x:
            return "–"
        s = f"{x:.{nd}f}"
        return s[1:] if s.startswith("-") and float(s) == 0 else s
    return str(x)


def table(head: list[str], rows: list[list]) -> list[str]:
    out = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    out += ["| " + " | ".join(str(c) for c in r) + " |" for r in rows]
    return out


def slot_status(lines: list[dict], data_root: pathlib.Path) -> tuple[dict[str, set[int]], bool]:
    """The PREREGISTRATION §9 skipped-slot inputs, built exactly as `python -m slwlab.analyze --data` builds
    them: (window_id -> slots with a block, from data/logs/<run>/blocklist.json; require a list for every
    confirmatory run). Missing lists for a confirmatory run stop the report, as they stop the analysis."""
    needed = blocklist.runs_with_purpose(lines, "measure")
    missing = [r for r in needed if blocklist.load(data_root, r) is None]
    if missing:
        raise SystemExit("no stored getBlocks list for run(s) " + ", ".join(missing)
                         + " (data/logs/<run_id>/blocklist.json): the report cannot classify their windows")
    present = blocklist.present_map(data_root, sorted({l["tx"]["run_id"] for l in lines}))
    return present, bool(needed)


def registered_analysis(lines: list[dict], sim: dict, data_root: pathlib.Path) -> dict:
    """The same call as the registered command (`python -m slwlab.analyze --data ...`), so the report's table
    is the committed results.json (tests/test_report_matches_registered.py compares them number by number)."""
    present, require = slot_status(lines, data_root)
    return analyze.analyze(lines, sim, seed=SEED, present=present, require_slot_status=require)


def measurement_section(lines: list[dict], sim: dict, data_root: pathlib.Path, cache: dict) -> list[str]:
    out = ["## MEASUREMENT: confirmatory results", ""]
    if not lines:
        return out + [NO_DATA + " `data/manifest.jsonl` has no mainnet, post-Alpenglow measurement lines.", ""]
    res = registered_analysis(lines, sim, data_root)
    present, require = slot_status(lines, data_root)
    windows = parse.windows_from_manifest(lines, present, require_slot_status=require)
    usable_dates: dict[tuple[str, str], set[str]] = defaultdict(set)
    for w in windows:
        if w.usable:
            bt = block_time(data_root, w.slot, cache)
            if bt is not None:
                usable_dates[(w.identity, w.arm)].add(utc_z(bt)[:10])
    out += ["Verdicts follow PREREGISTRATION §6–§9. tau_norm is the measured mean Kendall tau divided by",
            "the SIMULATION ceiling of its cell (see the SIMULATION section). `–` means not identifiable.",
            "UTC dates are the block times of the usable windows.", ""]
    rows = []
    for c in sorted(res["cells"], key=lambda c: (c["stratum"], c["identity"], c["arm"])):
        names = rules.RULES_BY_ARM[c["arm"]]
        best = max(names, key=lambda r: c["rules"][r]["mean_tau"]) if c["rules"] else None
        verdicts = ", ".join(f"{r}: {c['rules'][r]['verdict']}" for r in names) if c["rules"] else "no usable windows"
        rows.append([c["stratum"], f"`{c['identity'][:8]}…`", c["arm"], f"{c['windows_usable']}/{c['windows_sent']}",
                     f(c["landing_rate"]), best or "–",
                     f(c["rules"][best]["mean_tau"]) if best else "–",
                     f(c["rules"][best]["mean_tau_norm"]) if best else "–",
                     f(c["rules"][best]["p_holm"], 4) if best else "–",
                     verdicts, ", ".join(sorted(usable_dates[(c["identity"], c["arm"])])) or "–"])
    out += table(["stratum", "validator", "arm", "usable/sent windows", "landing", "top rule", "mean tau",
                  "tau_norm", "p (Holm)", "verdicts", "UTC dates"], rows)
    excl = Counter()
    for c in res["cells"]:
        excl.update(c["exclusions"])
    out += ["", "Tx outcomes (all cells): " + ", ".join(f"{k} {v}" for k, v in sorted(excl.items(), key=lambda kv: str(kv[0]))), ""]
    return out


def dates_section(lines: list[dict], data_root: pathlib.Path, cache: dict, label: str,
                  confirmatory: bool = False) -> list[str]:
    """When every window was measured (UTC). `confirmatory`: classify with the stored slot lists (§9), the way
    the analysis does, so "usable" here means what it means in the results."""
    if not lines:
        return [NO_DATA, ""]
    if confirmatory:
        present, require = slot_status(lines, data_root)
        windows = parse.windows_from_manifest(lines, present, require_slot_status=require)
    else:
        windows = parse.windows_from_manifest(lines)
    by_date: dict[str, Counter] = defaultdict(Counter)
    rows = []
    for w in windows:
        bt = block_time(data_root, w.slot, cache) if w.usable else None
        when = utc_z(bt) if bt is not None else run_start_utc(w.run_id)
        day = when[:10] if when != "–" else "–"
        by_date[day]["windows"] += 1
        by_date[day]["usable"] += int(w.usable)
        by_date[day]["txs"] += w.n
        rows.append([w.window_id, w.stratum, w.arm, w.slot if w.slot is not None else "–",
                     utc_z(bt) if bt is not None else f"– (run start {run_start_utc(w.run_id)})",
                     "usable" if w.usable else w.reason])
    out = table(["UTC date", "windows", "usable", "txs sent"],
                [[d, c["windows"], c["usable"], c["txs"]] for d, c in sorted(by_date.items())])
    # Windows by UTC hour of day. Sessions run only while the PC is on (daytime), so hours
    # without windows are shown as not measured, never filled (README "What this does NOT
    # separate"). A window without a block time (not landed) counts under "unknown".
    by_hour: dict[str, Counter] = defaultdict(Counter)
    for w in windows:
        bt = block_time(data_root, w.slot, cache) if w.usable else None
        h = utc_z(bt)[11:13] if bt is not None else "unknown"
        by_hour[h]["windows"] += 1
        by_hour[h]["usable"] += int(w.usable)
    hours = [f"{h:02d}" for h in range(24)] + (["unknown"] if "unknown" in by_hour else [])
    out += ["", "Windows by UTC hour of day (block time of the landed window):", ""]
    out += table(["UTC hour", "windows", "usable", ""],
                 [[h, by_hour[h]["windows"], by_hour[h]["usable"],
                   "not measured" if h != "unknown" and not by_hour[h]["windows"] else ""] for h in hours])
    out += ["", f"<details><summary>Every {label} window ({len(rows)}): slot and block time (UTC)</summary>", ""]
    out += table(["window", "stratum", "arm", "slot", "block time (UTC)", "status"], rows)
    out += ["", "</details>", ""]
    return out


def simulation_section(sim: dict, alloc: dict | None) -> list[str]:
    out = ["## SIMULATION reference (not measurements)", "",
           f"> SIMULATION. Seed {sim['seed']}, {sim['reps']} replications per cell. These numbers come from",
           "> `analysis/slwlab/sim.py` and were never observed on chain. Full tables: docs/SIMULATION.md.", "",
           "Tau ceiling: the mean tau of a validator that follows the rule exactly (medium nuisance).",
           f"Cells below {sim['min_ceiling']} are not identifiable (n/i) and can never be STRICT.", ""]
    names = sorted({r for fam in sim["ceilings"].values() for arm in fam.values() for r in arm})
    rows = []
    for fam in sorted(sim["ceilings"]):
        for arm in sorted(sim["ceilings"][fam]):
            cells = []
            for r in names:
                m = sim["ceilings"][fam][arm].get(r, {}).get("medium")
                cells.append("" if m is None else f(m["mean_tau"]) + (" n/i" if m["mean_tau"] < sim["min_ceiling"] else ""))
            rows.append([fam, arm] + cells)
    out += table(["scheduler model", "arm"] + names, rows)
    if alloc:
        a = alloc["allocation"]
        out += ["", "Window allocation (SIMULATION power; PREREGISTRATION §8):", ""]
        out += table(["group", "tier", "arm", "key rule", "W usable / validator", "power (half)", "cost (lamports)"],
                     [[r["group"], r["tier"], r["arm"], r["key_rule"], r["W_usable_per_validator"],
                       f(r["power_half"]), f"{r['cost_lamports']:,}"] for r in a["rows"]])
        out += ["", f"Confirmatory total {a['confirmatory_total_lamports']:,} lamports "
                    f"(budget {a['confirmatory_budget_lamports']:,})."]
    return out + [""]


def wrong_section(rows: list[dict]) -> list[str]:
    out = ["## What went wrong", "",
           "Built from docs/LOGBOOK.md (kinds: " + ", ".join(WRONG_KINDS) + "). Times are UTC.", ""]
    sel = [r for r in rows if r["kind"] in WRONG_KINDS]
    if not sel:
        return out + ["Nothing logged.", ""]
    counts = Counter(r["kind"] for r in sel)
    out += ["Counts: " + ", ".join(f"{k} {counts[k]}" for k in WRONG_KINDS if counts[k]), ""]
    out += table(["UTC", "phase", "kind", "entry"],
                 [[r["utc"] + (" †" if r["corrected"] else ""), r["phase"], r["kind"], r["entry"]] for r in sel])
    if any(r["corrected"] for r in sel):
        out += ["", "† Logged in local time (UTC+3) and converted here (LOGBOOK correction, 2026-10-22 21:30)."]
    return out + [""]


def render(root: pathlib.Path, sim_path: pathlib.Path, alloc_path: pathlib.Path) -> str:
    data_root = root / "data"
    inputs = {
        "data/manifest.jsonl": data_root / "manifest.jsonl",
        "data/exploratory/calibration_manifest.jsonl": data_root / "exploratory" / "calibration_manifest.jsonl",
        sim_path.relative_to(root).as_posix(): sim_path,
        alloc_path.relative_to(root).as_posix(): alloc_path,
        "docs/LOGBOOK.md": root / "docs" / "LOGBOOK.md",
    }
    measure, drop_m = split_scope(parse.read_jsonl(inputs["data/manifest.jsonl"]))
    measure = [l for l in measure if l["tx"].get("purpose") == "measure"]
    calib, drop_c = split_scope(parse.read_jsonl(inputs["data/exploratory/calibration_manifest.jsonl"]))
    sim = json.loads(sim_path.read_text(encoding="utf-8"))
    alloc = json.loads(alloc_path.read_text(encoding="utf-8")) if alloc_path.exists() else None
    cache: dict = {}

    out = ["# Report", "",
           "> Generated by `python -m slwlab.report` (`docker compose run --rm analyze`) from committed files.",
           "> Do not edit by hand. Deterministic: `python -m slwlab.report --check` regenerates it and compares.",
           "> Labels: **MEASUREMENT** (confirmatory, mainnet, post-Alpenglow), **EXPLORATORY** (calibration,",
           "> never used for verdicts), **SIMULATION** (model output, never observed).", ""]
    out += ["## Inputs", "", "SHA-256 of the LF-normalized file content.", ""]
    out += table(["file", "sha256", "lines"],
                 [[f"`{k}`", f"`{sha(p)}`" if p.exists() else "missing",
                   len(norm_bytes(p).splitlines()) if p.exists() else 0] for k, p in inputs.items()])
    out += ["", "Scope filters: `data/nonmainnet/` is never read. Lines from another cluster or before slot "
            f"{ALPENGLOW_GENESIS_SLOT:,} are dropped (manifest: "
            + (", ".join(f"{k} {v}" for k, v in sorted(drop_m.items())) or "none")
            + "; calibration: " + (", ".join(f"{k} {v}" for k, v in sorted(drop_c.items())) or "none") + ").", ""]

    out += measurement_section(measure, sim, data_root, cache)
    out += ["## MEASUREMENT: when (UTC)", ""] + dates_section(measure, data_root, cache, "confirmatory", confirmatory=True)
    out += ["## EXPLORATORY: calibration", "",
            "> EXPLORATORY. It is used only for order-agnostic nuisance estimates (`slwlab.calibrate`), never",
            "> for verdicts.", ""]
    out += dates_section(calib, data_root, cache, "calibration")
    out += simulation_section(sim, alloc)
    out += wrong_section(logbook_rows(inputs["docs/LOGBOOK.md"]))
    body = "\n".join(out).rstrip("\n") + "\n"
    return body + FOOTER + hashlib.sha256(body.encode("utf-8")).hexdigest() + " -->\n"


def self_hash_ok(text: str) -> bool:
    body, _, last = text.rstrip("\n").rpartition("\n")
    return last.startswith(FOOTER) and last[len(FOOTER):-4] == hashlib.sha256((body + "\n").encode("utf-8")).hexdigest()


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="docs/REPORT.md from committed data")
    ap.add_argument("--root", default=str(ROOT))
    ap.add_argument("--sim", default="analysis/sim/results/sim_v2.json")
    ap.add_argument("--alloc", default="analysis/sim/results/alloc_v2.json")
    ap.add_argument("--out", default="docs/REPORT.md")
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args(argv)
    root = pathlib.Path(a.root).resolve()
    text = render(root, root / a.sim, root / a.alloc)
    out = root / a.out
    if a.check:
        old = out.read_bytes().replace(b"\r\n", b"\n").decode("utf-8") if out.exists() else ""
        if old != text:
            print(f"{a.out} is stale or edited by hand: regenerate with python -m slwlab.report", file=sys.stderr)
            return 1
        print(f"{a.out} matches its inputs (sha256 {text.rstrip().rsplit(' ', 2)[-2]})")
        return 0
    out.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {a.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
