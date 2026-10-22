"""UTC-only timestamps (LOGBOOK 2026-10-22 21:30: local time once leaked into the logbook; the
operator's zone, Europe/Kyiv, also changes offset on 2026-10-25).

Accepted forms, both explicitly UTC:
  * RFC 3339 with `Z`:    2026-10-22T21:41:16Z, 2026-10-22T21:41:16.132024Z, 2026-10-22T21:00:00Z
  * compact ISO 8601 basic with `Z` (run ids, file names): 20261022T214116Z
Anything that looks like a date-time but is neither (an offset, no zone, a space separator)
is a violation. Used by tests/test_utc.py on data/ and by the report generator.
"""

from __future__ import annotations

import gzip
import json
import pathlib
import re

RFC3339_Z = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,9})?Z$")
COMPACT_Z = re.compile(r"\d{8}T\d{6}Z")
# anything date-time-like inside a string
LOOKS_LIKE = re.compile(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}[^\s\"',;)]*|\d{8}T\d{4,6}[^\s\"',;/._-]*")

# Third-party or chain data whose strings we did not write (block log messages, Kobe, gossip).
EXTERNAL = ("raw/blocks/", "snapshots/cluster_nodes_", "snapshots/kobe_validators_",
            "snapshots/vote_accounts_", "snapshots/leader_schedule_epoch")


def violations_in_string(s: str) -> list[str]:
    bad = []
    for m in LOOKS_LIKE.finditer(s):
        tok = m.group(0)
        if tok[:4].isdigit() and len(tok) > 8 and tok[4] == "-":
            if not RFC3339_Z.match(tok):
                bad.append(tok)
        elif not COMPACT_Z.fullmatch(tok):
            bad.append(tok)
    return bad


def walk(v, path: str = ""):
    if isinstance(v, dict):
        for k, x in v.items():
            yield from walk(k, f"{path}.<key>")
            yield from walk(x, f"{path}.{k}")
    elif isinstance(v, list):
        for i, x in enumerate(v):
            yield from walk(x, f"{path}[{i}]")
    elif isinstance(v, str):
        for tok in violations_in_string(v):
            yield path, tok


def scan_file(p: pathlib.Path) -> list[tuple[str, str]]:
    opener = gzip.open if p.suffix == ".gz" else open
    name = p.name.removesuffix(".gz")
    with opener(p, "rt", encoding="utf-8") as f:
        if name.endswith(".jsonl"):
            docs = [json.loads(l) for l in f if l.strip()]
        elif name.endswith(".json"):
            docs = [json.load(f)]
        else:
            return []
    out = [(p.name + path, tok) for d in docs for path, tok in walk(d)]
    out += [(p.name, tok) for tok in violations_in_string(name)]
    return out


def scan_tree(root: pathlib.Path) -> list[tuple[str, str]]:
    out = []
    for p in sorted(root.rglob("*")):
        rel = p.relative_to(root).as_posix()
        if not p.is_file() or any(e in rel for e in EXTERNAL) or ".ratelimit" in rel:
            continue
        if p.name.endswith((".json", ".jsonl", ".json.gz", ".jsonl.gz")):
            out += [(rel + ": " + w, tok) for w, tok in scan_file(p)]
    return out
