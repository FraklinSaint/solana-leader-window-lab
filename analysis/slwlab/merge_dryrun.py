"""EXPLORATORY: merge session chunks into one clock-error and coverage report by UTC hour.

``python -m slwlab.merge_dryrun --since 20261023T000000Z [--until ...]`` (run from analysis/;
``make dryrun-merge SINCE=...``).

The PC is on only in the daytime, so the 24 h dry-run is a set of chunks (sessions) on
different dates. Input: every ``data/logs/dryrun-<stamp>/`` and ``data/logs/live-<stamp>/`` with
since <= stamp < until, plus the session event logs ``data/sessions/<id>/events.jsonl``
(``sender session``).

For each run: its mode (dry-run, or live calibration/confirmatory) and platform (``run.json``
``host``: platform id, OS, machine id; runs from before host records are ``unrecorded``), its live interval (``run.json`` start to ``summary.json`` finish; if the
process died without a summary, the last timestamp it wrote, marked ``end_inferred``), its
blind intervals (``gaps.jsonl``), and its window-start prediction errors (``clock.jsonl``
``err_prep_ms``, the same metric and caveat as ``sender clock-report``).

Output, per (mode, platform, RPC host) and pooled:
* per absolute UTC hour: covered seconds (live minus blind), error quantiles, windows;
* per hour of day (00..23, over all dates): covered seconds, which dates and platforms
  contributed, and a flag: ``GAP`` (no coverage), ``THIN`` (< 1800 s), ``OK``.

Gaps are reported, never filled: an hour without data has ``err: null`` and is flagged. No
value is interpolated, carried over from another hour, or borrowed from another platform.
Dry-run chunks are EXPLORATORY. The hours a session did not cover are exactly the hours the
measurement does not speak for (README "What this does NOT separate").
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import re
from collections import defaultdict

HOUR_NS = 3_600 * 10**9
THIN_SECS = 1_800
STAMP = re.compile(r"^\d{8}T\d{6}Z$")


def parse_utc(s: str) -> int:
    """RFC 3339 with Z (fractional seconds allowed) -> unix ns."""
    if not s.endswith("Z"):
        raise ValueError(f"not UTC: {s}")
    main, _, frac = s[:-1].partition(".")
    t = dt.datetime.strptime(main, "%Y-%m-%dT%H:%M:%S").replace(tzinfo=dt.timezone.utc)
    return int(t.timestamp()) * 10**9 + int((frac + "000000000")[:9] or 0)


def fmt_hour(ns: int) -> str:
    return dt.datetime.fromtimestamp(ns // 10**9, tz=dt.timezone.utc).strftime("%Y-%m-%dT%H:00:00Z")


def read_jsonl(p: pathlib.Path) -> list[dict]:
    if not p.exists():
        return []
    out = []
    for line in p.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            out.append(json.loads(line))
        except ValueError:
            # A process killed mid-write leaves a torn last line: skip it, count it.
            out.append({"_torn": True})
    return out


def platform_of(run: dict) -> str:
    """Host platform plus RPC host: the slot feed is part of what the clock error measures."""
    h = run.get("host")
    host = f"{h.get('platform')}/{h.get('os')}/{h.get('machine_id')}" if h else "unrecorded"
    ep = (run.get("run_config") or {}).get("endpoint") or ""
    rpc = ep.split("://", 1)[-1].split("/", 1)[0].split("?", 1)[0].split(":", 1)[0] or "unknown-rpc"
    mode = run.get("mode") or "unknown"
    if mode == "live":
        mode = f"live-{run.get('purpose') or 'unknown'}"
    return f"{mode} | {host} via {rpc}"


def load_run(d: pathlib.Path) -> dict | None:
    try:
        run = json.loads((d / "run.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if run.get("cluster") != "mainnet" or run.get("mode") not in ("dry_run", "live"):
        return None
    start = parse_utc(run["started"])
    torn = 0
    clock = read_jsonl(d / "clock.jsonl")
    wins = read_jsonl(d / "windows.jsonl")
    logs = read_jsonl(d / "run.log.jsonl")
    gaps = read_jsonl(d / "gaps.jsonl")
    for xs in (clock, wins, logs, gaps):
        torn += sum(1 for x in xs if x.get("_torn"))
    clock = [c for c in clock if not c.get("_torn")]
    wins = [w for w in wins if not w.get("_torn")]
    gaps = [g for g in gaps if not g.get("_torn")]
    end, inferred = None, False
    try:
        s = json.loads((d / "summary.json").read_text(encoding="utf-8"))
        end = parse_utc(s["finished"]) if s.get("finished") else None
    except (OSError, ValueError, KeyError):
        pass
    if end is None:
        inferred = True
        seen = [start]
        seen += [int(c["observed_unix_ns"]) for c in clock if c.get("observed_unix_ns")]
        seen += [parse_utc(w["ts"]) for w in wins if w.get("ts")]
        seen += [parse_utc(l["timestamp"]) for l in logs if not l.get("_torn") and l.get("timestamp")]
        end = max(seen)
    return {
        "run_id": d.name, "platform": platform_of(run), "host": run.get("host"),
        "start": start, "end": end, "end_inferred": inferred, "torn_lines": torn,
        "gaps": [(int(g["start_unix_ns"]), int(g["end_unix_ns"]), g["kind"]) for g in gaps],
        "errs": [(int(c.get("predicted_prep_unix_ns") or c["predicted_plan_unix_ns"]),
                  c.get("err_prep_ms")) for c in clock
                 if c.get("predicted_prep_unix_ns") or c.get("predicted_plan_unix_ns")],
        "windows": [(int(w["window"]["send_at_unix_ns"]), w.get("outcome")) for w in wins
                    if w.get("window", {}).get("send_at_unix_ns")],
    }


def union(iv: list[tuple[int, int]]) -> list[tuple[int, int]]:
    out: list[list[int]] = []
    for a, b in sorted(iv):
        if b <= a:
            continue
        if out and a <= out[-1][1]:
            out[-1][1] = max(out[-1][1], b)
        else:
            out.append([a, b])
    return [(a, b) for a, b in out]


def subtract(iv: list[tuple[int, int]], cut: list[tuple[int, int]]) -> list[tuple[int, int]]:
    out = []
    cut = union(cut)
    for a, b in iv:
        s = a
        for c0, c1 in cut:
            if c1 <= s or c0 >= b:
                continue
            if c0 > s:
                out.append((s, c0))
            s = max(s, c1)
        if s < b:
            out.append((s, b))
    return out


def per_hour_secs(iv: list[tuple[int, int]]) -> dict[int, float]:
    out: dict[int, float] = defaultdict(float)
    for a, b in iv:
        s = a
        while s < b:
            h = s // HOUR_NS * HOUR_NS
            e = min(b, h + HOUR_NS)
            out[h] += (e - s) / 1e9
            s = e
    return out


def quantile(xs: list[float], q: float) -> float:
    xs = sorted(xs)
    i = q * (len(xs) - 1)
    lo = int(i)
    hi = min(lo + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (i - lo)


def err_stats(errs: list[float]) -> dict | None:
    if not errs:
        return None
    a = [abs(e) for e in errs]
    return {"n": len(a), "median_abs_ms": round(quantile(a, 0.5), 3), "p90_abs_ms": round(quantile(a, 0.9), 3),
            "p95_abs_ms": round(quantile(a, 0.95), 3), "max_abs_ms": round(max(a), 3),
            "mean_ms": round(sum(errs) / len(errs), 3),
            "share_within_100ms": round(sum(1 for e in a if e <= 100) / len(a), 4)}


def summarize(runs: list[dict]) -> dict:
    live = union([(r["start"], r["end"]) for r in runs])
    blind = [(a, b) for r in runs for a, b, _ in r["gaps"]]
    covered = subtract(live, blind)
    cov = per_hour_secs(covered)
    blind_by_kind: dict[int, dict[str, float]] = defaultdict(lambda: defaultdict(float))
    for r in runs:
        for a, b, k in r["gaps"]:
            for h, s in per_hour_secs([(a, b)]).items():
                blind_by_kind[h][k] += s
    errs: dict[int, list[float]] = defaultdict(list)
    unobserved: dict[int, int] = defaultdict(int)
    for r in runs:
        for t, e in r["errs"]:
            h = t // HOUR_NS * HOUR_NS
            if e is None:
                unobserved[h] += 1
            else:
                errs[h].append(float(e))
    executed: dict[int, int] = defaultdict(int)
    skipped: dict[int, int] = defaultdict(int)
    for r in runs:
        for t, o in r["windows"]:
            h = t // HOUR_NS * HOUR_NS
            (skipped if o == "skipped" else executed)[h] += 1
    hours = sorted(set(cov) | set(errs) | set(blind_by_kind))
    if hours:
        hours = list(range(hours[0], hours[-1] + HOUR_NS, HOUR_NS))  # empty hours stay visible
    rows = [{"hour_utc": fmt_hour(h), "covered_secs": round(cov.get(h, 0.0), 1),
             "blind_secs": {k: round(v, 1) for k, v in sorted(blind_by_kind.get(h, {}).items())},
             "windows_executed": executed.get(h, 0), "windows_skipped": skipped.get(h, 0),
             "unobserved": unobserved.get(h, 0), "err": err_stats(errs.get(h, []))} for h in hours]
    hod: dict[int, dict] = {i: {"covered_secs": 0.0, "dates": set(), "errs": []} for i in range(24)}
    for h, s in cov.items():
        k = (h // HOUR_NS) % 24
        hod[k]["covered_secs"] += s
        if s > 0:
            hod[k]["dates"].add(fmt_hour(h)[:10])
    for h, es in errs.items():
        hod[(h // HOUR_NS) % 24]["errs"] += es
    by_hod = []
    for k in range(24):
        c = hod[k]["covered_secs"]
        by_hod.append({"hour_of_day_utc": f"{k:02d}", "covered_secs": round(c, 1),
                       "dates": sorted(hod[k]["dates"]), "err": err_stats(hod[k]["errs"]),
                       "flag": "GAP" if c == 0 else ("THIN" if c < THIN_SECS else "OK")})
    return {"hours": rows, "by_hour_of_day": by_hod,
            "covered_secs_total": round(sum(cov.values()), 1),
            "flags": {f: [r["hour_of_day_utc"] for r in by_hod if r["flag"] == f] for f in ("GAP", "THIN")}}


def campaign_events(data: pathlib.Path, since: str, until: str | None) -> list[dict]:
    out = []
    files = [(p.parent.name, p) for p in sorted((data / "sessions").glob("*/events.jsonl"))]
    for st, p in files:
        if st >= since and (until is None or st < until):
            for e in read_jsonl(p):
                if not e.get("_torn"):
                    out.append({"file": str(p.relative_to(data)).replace("\\", "/"), **e})
    return out


def merge(data: pathlib.Path, since: str, until: str | None) -> dict:
    for s in (since, until):
        if s is not None and not STAMP.match(s):
            raise SystemExit(f"stamps are compact UTC, e.g. 20261023T000000Z (got {s})")
    logs = data / "logs"
    runs, skipped = [], []
    for d in sorted([*logs.glob("dryrun-*"), *logs.glob("live-*")], key=lambda d: d.name.split("-", 1)[1]):
        st = d.name.split("-", 1)[1]
        if not d.is_dir() or st < since or (until is not None and st >= until):
            continue
        r = load_run(d)
        (runs.append(r) if r else skipped.append(d.name))
    if not runs:
        raise SystemExit(f"no mainnet dry-run or live directories since {since} in {logs}")
    by_platform: dict[str, list[dict]] = defaultdict(list)
    for r in runs:
        by_platform[r["platform"]].append(r)
    overlaps = []
    plats = sorted(by_platform)
    for i, p in enumerate(plats):
        for q in plats[i + 1:]:
            a = union([(r["start"], r["end"]) for r in by_platform[p]])
            b = union([(r["start"], r["end"]) for r in by_platform[q]])
            both = sum(min(x1, y1) - max(x0, y0) for x0, x1 in a for y0, y1 in b if min(x1, y1) > max(x0, y0))
            if both:
                overlaps.append({"platforms": [p, q], "secs": round(both / 1e9, 1)})
    return {
        "label": "EXPLORATORY",
        "metric": "window-start prediction error at prep time (clock.jsonl err_prep_ms): arrival of the "
                  "window's first slot notification at the sending host minus the prediction. Not the "
                  "leader's real block start. Hours without data are flagged, never filled.",
        "since": since, "until": until,
        "runs": [{k: r[k] for k in ("run_id", "platform", "end_inferred", "torn_lines")}
                 | {"start_utc": fmt_ns(r["start"]), "end_utc": fmt_ns(r["end"]),
                    "kernel": (r["host"] or {}).get("kernel"),
                    "sender_sha256": (r["host"] or {}).get("sender_sha256")} for r in runs],
        "ignored_dirs": skipped,
        "platform_overlap": overlaps,
        "by_platform": {p: summarize(rs) for p, rs in sorted(by_platform.items())},
        "pooled": summarize(runs),
        "campaign_events": campaign_events(data, since, until),
    }


def fmt_ns(ns: int) -> str:
    t = dt.datetime.fromtimestamp(ns // 10**9, tz=dt.timezone.utc)
    return t.strftime("%Y-%m-%dT%H:%M:%SZ")


def markdown(m: dict) -> str:
    L = [f"# Dry-run merge {m['since']} .. {m['until'] or 'now'} (EXPLORATORY)", "",
         f"> {m['metric']}", "", "| run | platform | start | end | end inferred |", "|---|---|---|---|---|"]
    L += [f"| {r['run_id']} | {r['platform']} | {r['start_utc']} | {r['end_utc']} | "
          f"{'yes' if r['end_inferred'] else ''} |" for r in m["runs"]]
    for name, s in [("pooled (all platforms)", m["pooled"])] + [(p, s) for p, s in m["by_platform"].items()]:
        L += ["", f"## Coverage by hour of day: {name}", "",
              f"Covered: {s['covered_secs_total'] / 3600:.1f} h. GAP hours: {', '.join(s['flags']['GAP']) or 'none'}. "
              f"THIN (< {THIN_SECS // 60} min): {', '.join(s['flags']['THIN']) or 'none'}.", "",
              "| hour (UTC) | covered (min) | dates | n | median abs (ms) | p95 abs (ms) | flag |",
              "|---|---|---|---|---|---|---|"]
        for r in s["by_hour_of_day"]:
            e = r["err"] or {}
            L.append(f"| {r['hour_of_day_utc']} | {r['covered_secs'] / 60:.0f} | {', '.join(r['dates'])} | "
                     f"{e.get('n', 0)} | {e.get('median_abs_ms', '—')} | {e.get('p95_abs_ms', '—')} | {r['flag']} |")
    if m["platform_overlap"]:
        L += ["", "Platforms overlapping in time (each kept separately above): " +
              "; ".join(f"{' & '.join(o['platforms'])}: {o['secs'] / 60:.0f} min" for o in m["platform_overlap"])]
    return "\n".join(L) + "\n"


def main(argv: list[str] | None = None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--data", default="../data")
    ap.add_argument("--since", required=True, help="compact UTC stamp, e.g. 20261023T000000Z")
    ap.add_argument("--until", default=None)
    ap.add_argument("--out", default=None, help="default: <data>/exploratory/dryrun_merged-<since>.json")
    a = ap.parse_args(argv)
    data = pathlib.Path(a.data)
    m = merge(data, a.since, a.until)
    out = pathlib.Path(a.out) if a.out else data / "exploratory" / f"dryrun_merged-{a.since}.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(m, indent=1, sort_keys=True) + "\n", encoding="utf-8", newline="\n")
    out.with_suffix(".md").write_text(markdown(m), encoding="utf-8", newline="\n")
    print(f"wrote {out} and {out.with_suffix('.md')}")


if __name__ == "__main__":
    main()
