#!/usr/bin/env python3
"""Receive the data of operator B (AMENDMENT 3, platform B). Read-only towards our data/; prints counts and paths only.

    python scripts/friend_data_check.py DATA_B.zip            # extract to data_platform_b/<UTC stamp>/data and check
    python scripts/friend_data_check.py --dir data_platform_b/<stamp>/data    # check an extracted directory

Checks, in this order (exit 1 on any failure; nothing is merged into data/):
 1. every member of the zip is under `data/` (no secrets/, no .env, no absolute path, no `..`);
 2. no provider API key value, no 64-number array that is a real keypair, no 87/88-char base58 string that is a keypair
    (the tests of scripts/check_no_keys.py, applied to the extracted tree);
 3. scripts/verify_manifest.py --data <dir> (offline checks; check Q: ONE platform, ONE machine, ONE network type);
 4. counts: platforms in the manifests, sessions, windows and txs per purpose.
It never imports analysis code and never writes outside data_platform_b/.
"""

from __future__ import annotations

import argparse
import datetime as dt
import gzip
import importlib.util
import json
import pathlib
import re
import subprocess
import sys
import zipfile
from collections import Counter

ROOT = pathlib.Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("check_no_keys", ROOT / "scripts" / "check_no_keys.py")
cnk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cnk)
ARRAY = re.compile(r"\[\s*(?:\d{1,3}\s*,\s*){63}\d{1,3}\s*\]")
MAX_MEMBER = 64 * 1024 * 1024


def extract(zpath: pathlib.Path) -> pathlib.Path:
    dest = ROOT / "data_platform_b" / dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    with zipfile.ZipFile(zpath) as z:
        bad = [i.filename for i in z.infolist()
               if i.filename.startswith(("/", "\\")) or ".." in pathlib.PurePosixPath(i.filename).parts
               or not (i.filename.replace("\\", "/").startswith("data/") or i.is_dir() and i.filename.rstrip("/") == "data")]
        if bad:
            print(f"FAIL 1: {len(bad)} zip member(s) outside data/: {sorted(bad)[:10]}")
            raise SystemExit(1)
        z.extractall(dest)
    print(f"extracted {zpath.name} to {dest.relative_to(ROOT)} (all members under data/)")
    return dest / "data"


def scan(data: pathlib.Path) -> int:
    bad = 0
    n = 0
    for p in sorted(data.rglob("*")):
        if not p.is_file():
            continue
        n += 1
        rel = p.relative_to(data).as_posix()
        if p.name.startswith(".env") or p.name in ("master.json", "sink.json", "id.json") or p.name.endswith(".keypair.json"):
            print(f"FAIL 2: forbidden file name {rel}")
            bad += 1
            continue
        raw = p.read_bytes()
        if p.suffix == ".gz":
            try:
                raw = gzip.decompress(raw)
            except OSError:
                continue
        text = raw.decode("utf-8", errors="replace")
        if any(m.group(1) != "REDACTED" for m in cnk.API_KEY.finditer(text)):
            print(f"FAIL 2: provider API key in a URL parameter: {rel}")
            bad += 1
        for m in ARRAY.finditer(text):
            vals = json.loads(m.group(0))
            if all(0 <= v <= 255 for v in vals) and cnk.ed25519_public(bytes(vals[:32])) == bytes(vals[32:]):
                print(f"FAIL 2: real keypair array in {rel}")
                bad += 1
                break
        if cnk.has_keypair_b58(text):
            print(f"FAIL 2: base58 keypair in {rel}")
            bad += 1
    print(f"scanned {n} files: {bad} secret finding(s)")
    return bad


def counts(data: pathlib.Path) -> None:
    plats: Counter = Counter()
    purposes: Counter = Counter()
    wins: dict[str, set] = {}
    for m in (data / "manifest.jsonl", data / "exploratory" / "calibration_manifest.jsonl"):
        if not m.exists():
            continue
        for line in m.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            x = json.loads(line)
            h = x["tx"].get("host") or {}
            plats[f"{h.get('platform')}/{h.get('os')}/{h.get('machine_id')}/{h.get('network_type')}"] += 1
            purposes[x["tx"]["purpose"]] += 1
            wins.setdefault(x["tx"]["purpose"], set()).add(x["tx"]["window_id"])
    print("txs per platform/os/machine/network:", dict(plats))
    print("txs per purpose:", dict(purposes), " windows per purpose:", {k: len(v) for k, v in wins.items()})
    print("sessions:", len([d for d in (data / "sessions").glob("*") if d.is_dir()]) if (data / "sessions").exists() else 0)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("zip", nargs="?")
    ap.add_argument("--dir")
    a = ap.parse_args(argv)
    if bool(a.zip) == bool(a.dir):
        ap.error("give a zip or --dir")
    data = pathlib.Path(a.dir) if a.dir else extract(pathlib.Path(a.zip))
    bad = scan(data)
    if bad:
        print("RESULT: FAIL (secrets); do not keep or share this directory")
        return 1
    r = subprocess.run([sys.executable, "-I", str(ROOT / "scripts" / "verify_manifest.py"), "--data", str(data)], capture_output=True, text=True)
    print("verify_manifest exit", r.returncode)
    print("\n".join((r.stdout + r.stderr).strip().splitlines()[-12:]))
    counts(data)
    print("RESULT:", "OK" if r.returncode == 0 else "FAIL (verify_manifest)")
    return r.returncode


if __name__ == "__main__":
    sys.exit(main())
