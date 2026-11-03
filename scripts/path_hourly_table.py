"""PREREGISTRATION section 6d, last sentence: per UTC hour, the RTT jitter and signal statistics
next to the window-start prediction error ("clock error"), for the confirmatory (measure) sessions.

DESCRIPTIVE DATA QUALITY. Reads only the session path record (rtt/wifi/network/coverage); it never
opens the manifest, a block or an order, and computes no tau, p-value or verdict.

    python scripts/path_hourly_table.py --data data [--session ID ...] [--out-json F] [--out-txt F]

Per session (default: every data/sessions/<id> whose session.json has mode "measure") and UTC hour:
  rtt    network.json rtt_by_hour, copied as written by the sender (n, loss_share, p50, p95, jitter, sigma_mad)
  signal signal_quality of the 30 s wifi.jsonl samples of that hour, nearest-rank min/p5/median/p95/max
         (same method as crates/sender/src/netmon.rs `percentile`/`spread`, which produced the session
         totals in network.json)
  clock  coverage.json hours[].clock_err (err_prep_ms distribution of the windows of that hour, written by
         the sender from clock.jsonl: median_abs, p90_abs, p95_abs, max_abs)
  also covered_secs, flag and windows executed/skipped from coverage.json.
A hour missing from one source stays None (shown "-"); nothing is filled, interpolated or borrowed.
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib


def read_json(p: pathlib.Path):
    return json.loads(p.read_text(encoding="utf-8"))


def read_jsonl(p: pathlib.Path) -> list[dict]:
    if not p.exists():
        return []
    return [json.loads(l) for l in p.read_text(encoding="utf-8").splitlines() if l.strip()]


def nearest_rank(sorted_v: list[float], q: float):
    """Same as netmon.rs `percentile`: rank = ceil(q * n), at least 1; None for an empty list."""
    if not sorted_v:
        return None
    rank = max(1, math.ceil(q * len(sorted_v)))
    return sorted_v[min(rank, len(sorted_v)) - 1]


def spread(values: list[float]) -> dict | None:
    v = sorted(values)
    if not v:
        return None
    return {"n": len(v), "min": v[0], "p5": nearest_rank(v, 0.05), "median": nearest_rank(v, 0.5),
            "p95": nearest_rank(v, 0.95), "max": v[-1]}


def hour_of(ts: str) -> str:
    """'2026-11-02T12:38:39Z' -> '2026-11-02T12:00:00Z'."""
    return ts[:13] + ":00:00Z"


def signal_by_hour(wifi_lines: list[dict]) -> dict[str, dict]:
    """Hour -> signal_quality spread. Samples without a readable link (wifi null) are skipped,
    as the sender's own summary does."""
    by: dict[str, list[float]] = {}
    for w in wifi_lines:
        link = w.get("wifi")
        if not link or link.get("signal_quality") is None or not w.get("t"):
            continue
        by.setdefault(hour_of(w["t"]), []).append(float(link["signal_quality"]))
    return {h: spread(v) for h, v in by.items()}


RTT_KEYS = ("n", "n_err", "loss_share", "p50_ms", "p95_ms", "p99_ms", "jitter_ms", "sigma_mad_ms", "max_ms")
CLOCK_KEYS = ("median_abs_ms", "p90_abs_ms", "p95_abs_ms", "max_abs_ms")


def session_rows(sdir: pathlib.Path) -> list[dict]:
    net = read_json(sdir / "network.json")
    cov = read_json(sdir / "coverage.json")
    rtt = {r["hour_utc"]: {k: r.get(k) for k in RTT_KEYS} for r in net.get("rtt_by_hour", [])}
    sig = signal_by_hour(read_jsonl(sdir / "wifi.jsonl"))
    cov_h = {h["hour_utc"]: h for h in cov.get("hours", [])}
    rows = []
    for hour in sorted(set(rtt) | set(sig) | set(cov_h)):
        c = cov_h.get(hour)
        ce = (c or {}).get("clock_err")
        clock = None
        if ce:
            clock = {k: ce.get(k) for k in CLOCK_KEYS}
        rows.append({
            "hour_utc": hour,
            "covered_secs": c.get("covered_secs") if c else None,
            "flag": c.get("flag") if c else None,
            "windows_executed": c.get("windows_executed") if c else None,
            "windows_skipped": c.get("windows_skipped") if c else None,
            "rtt": rtt.get(hour),
            "signal_quality": sig.get(hour),
            "clock_err": clock,
        })
    return rows


def measure_sessions(data: pathlib.Path) -> list[str]:
    out = []
    for d in sorted((data / "sessions").iterdir()):
        sj = d / "session.json"
        if sj.exists() and read_json(sj).get("mode") == "measure":
            out.append(d.name)
    return out


def fmt(x, nd=1):
    return "-" if x is None else (f"{x:.{nd}f}" if isinstance(x, float) else str(x))


def render(report: dict) -> str:
    head = ["hour_utc", "flag", "cov_s", "win", "rtt_n", "loss", "rtt_p50", "rtt_p95", "jitter", "sig_min", "sig_p5",
            "sig_med", "sig_p95", "err_med", "err_p90", "err_p95", "err_max"]
    lines = ["PATH AND CLOCK BY UTC HOUR (DESCRIPTIVE DATA QUALITY; no ordering information used)",
             "rtt in ms; signal_quality in %; err = |err_prep_ms| (window-start prediction vs arrival of the first slot",
             "notification at this machine; NOT the leader's real block start); '-' = no data for that hour", ""]
    for sid, rows in report["sessions"].items():
        lines += [f"session {sid}", "\t".join(head)]
        for r in rows:
            rt, sg, ck = r["rtt"] or {}, r["signal_quality"] or {}, r["clock_err"] or {}
            lines.append("\t".join([
                r["hour_utc"], fmt(r["flag"]), fmt(r["covered_secs"]), fmt(r["windows_executed"]),
                fmt(rt.get("n")), fmt(rt.get("loss_share"), 4), fmt(rt.get("p50_ms")), fmt(rt.get("p95_ms")),
                fmt(rt.get("jitter_ms")), fmt(sg.get("min")), fmt(sg.get("p5")), fmt(sg.get("median")),
                fmt(sg.get("p95")), fmt(ck.get("median_abs_ms")),
                fmt(ck.get("p90_abs_ms")), fmt(ck.get("p95_abs_ms")), fmt(ck.get("max_abs_ms"))]))
        lines.append("")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--data", default="data")
    ap.add_argument("--session", action="append", help="session id (repeatable); default: all measure sessions")
    ap.add_argument("--out-json")
    ap.add_argument("--out-txt")
    a = ap.parse_args(argv)
    data = pathlib.Path(a.data)
    ids = a.session or measure_sessions(data)
    report = {"label": "DESCRIPTIVE DATA QUALITY (PREREGISTRATION 6d)", "sessions": {}}
    for sid in ids:
        report["sessions"][sid] = session_rows(data / "sessions" / sid)
    txt = render(report)
    for opt, content in ((a.out_json, json.dumps(report, indent=1, sort_keys=True) + "\n"), (a.out_txt, txt + "\n")):
        if opt:
            p = pathlib.Path(opt)
            if p.exists():
                raise SystemExit(f"{p} exists: write-once, choose a new name")
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(content, encoding="utf-8", newline="\n")
    print(txt)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
