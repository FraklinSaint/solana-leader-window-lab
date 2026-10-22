"""Every timestamp we write is UTC: RFC 3339 with Z (or compact ISO basic with Z in ids and file
names). Europe/Kyiv changes offset on 2026-10-25, and local time once leaked into the LOGBOOK
(2026-10-22 21:30 correction), so local time must never appear in data/ or in our code paths."""

import datetime as dt
import pathlib
import re

from slwlab import utc

ROOT = pathlib.Path(__file__).resolve().parents[2]

# Written before this test existed. Both values are explicitly UTC but not in the strict form;
# the files are committed raw data and are not rewritten. Their generators are fixed.
GRANDFATHERED = {
    ("snapshots/facts_20261022T151615Z.json", "2026-10-22T15:16:15.183170+00:00"),  # verify_facts isoformat()
    ("nonmainnet/devnet/logs/dryrun-20261022T191511Z/clock_hourly.json", "2026-10-22T19:00Z"),  # gaps.rs hour bucket
}


def test_scanner_accepts_utc_and_rejects_everything_else():
    for ok in ("2026-10-22T21:41:16Z", "2026-10-22T21:41:16.132024Z", "dryrun-20261022T183752Z",
               "x 2026-10-22T21:00:00Z y", "facts_20261022T151615Z.json", "slot 460188407"):
        assert utc.violations_in_string(ok) == [], ok
    for bad in ("2026-10-22T21:41:16+03:00", "2026-10-22T21:41:16+00:00", "2026-10-22 21:41:16",
                "2026-10-22T21:41:16", "2026-10-22T19:00Z", "run-20261022T183752", "20261022T2141"):
        assert utc.violations_in_string(bad), bad


def test_committed_data_is_utc():
    found = utc.scan_tree(ROOT / "data")
    left = [(w, t) for w, t in found if not any(w.startswith(f + ":") and t == g for f, g in GRANDFATHERED)]
    assert left == [], left[:10]


FORBIDDEN = {
    ".rs": [r"chrono::Local", r"\bLocal::now", r"naive_local", r"with_timezone\(&Local"],
    ".py": [r"datetime\.now\(\)", r"\.now\(\s*\)", r"utcnow\(", r"time\.localtime", r"time\.strftime",
            r"fromtimestamp\((?![^)]*tz)", r"\.isoformat\("],
    ".ps1": [r"Get-Date(?!\)\.ToUniversalTime\(\))"],
}


def test_source_never_formats_local_time():
    files = [*ROOT.glob("crates/*/src/**/*.rs"), *ROOT.glob("crates/*/examples/*.rs"),
             *ROOT.glob("scripts/*.py"), *ROOT.glob("scripts/*.ps1"), *ROOT.glob("analysis/slwlab/*.py")]
    hits = []
    for f in files:
        pats = FORBIDDEN[f.suffix]
        for n, line in enumerate(f.read_text(encoding="utf-8").splitlines(), 1):
            code = line.split("//")[0] if f.suffix == ".rs" else line.split("#")[0]
            hits += [f"{f.relative_to(ROOT)}:{n}: {line.strip()}" for p in pats if re.search(p, code)]
    assert hits == [], hits


# Rows stamped in local time (UTC+3) before the 2026-10-22 21:30 correction entry.
CORRECTED_ROWS = {"2026-10-23 00:30", "2026-10-23 00:35"}
ROW = re.compile(r"^\| (\d{4}-\d{2}-\d{2} \d{2}:\d{2}) \|")


def logbook_stamps():
    rows = []
    for line in (ROOT / "docs" / "LOGBOOK.md").read_text(encoding="utf-8").splitlines():
        if line.startswith("| 20"):
            m = ROW.match(line)
            assert m, f"malformed LOGBOOK row: {line[:60]}"
            rows.append(m.group(1))
    return rows


def test_logbook_stamps_are_utc_ordered_and_not_in_the_future():
    rows = logbook_stamps()
    assert rows
    now = dt.datetime.now(dt.timezone.utc) + dt.timedelta(minutes=5)
    last = None
    for s in rows:
        t = dt.datetime.strptime(s, "%Y-%m-%d %H:%M").replace(tzinfo=dt.timezone.utc)
        if s in CORRECTED_ROWS:
            continue
        # a local-time stamp (UTC+2/+3) written now would lie in the future
        assert t <= now, f"LOGBOOK row {s} is in the future: local time instead of UTC?"
        assert last is None or t >= last, f"LOGBOOK row {s} is earlier than the row before it ({last:%Y-%m-%d %H:%M})"
        last = t
