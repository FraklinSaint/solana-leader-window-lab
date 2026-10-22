#!/usr/bin/env python3
"""Fail if any tracked file looks like it contains a Solana secret key.

Checks every `git ls-files` path for:
  * a JSON array of exactly 64 integers in 0..255 (solana-keygen keypair format)
  * an 87/88-char base58 string (64-byte secret key encoding)
  * anything tracked under secrets/ other than README.md and .gitkeep
  * a provider API key in a URL parameter (api-key=..., api_key=..., apikey=...), in any
    tracked file and in every file under data/ (tracked or not; .gz decompressed)
"""
import gzip
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
ARRAY = re.compile(r"\[\s*(?:\d{1,3}\s*,\s*){63}\d{1,3}\s*\]")
B58_SECRET = re.compile(r"(?<![1-9A-HJ-NP-Za-km-z])[1-9A-HJ-NP-Za-km-z]{87,88}(?![1-9A-HJ-NP-Za-km-z])")
# Same rule as crates/sender/src/redact.rs::contains_api_key.
API_KEY = re.compile(r"(?i)(?:x-)?api[-_]?key=([A-Za-z0-9_-]{8,})(?![A-Za-z0-9_-])")
ALLOWED_IN_SECRETS = {"secrets/README.md", "secrets/.gitkeep"}
# Raw blocks contain 88-char base58 *signatures*; those are public and expected.
B58_EXEMPT_PREFIXES = ("data/",)


def main() -> int:
    files = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, capture_output=True, check=True).stdout
    bad = []
    for rel in filter(None, files.decode().split("\0")):
        if rel.startswith("secrets/") and rel not in ALLOWED_IN_SECRETS:
            bad.append(f"{rel}: tracked file under secrets/")
            continue
        path = ROOT / rel
        if path.suffix == ".gz" or not path.is_file():
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        for m in ARRAY.finditer(text):
            if all(0 <= v <= 255 for v in json.loads(m.group(0))):
                bad.append(f"{rel}: 64-byte integer array (keypair format)")
                break
        if not rel.startswith(B58_EXEMPT_PREFIXES) and B58_SECRET.search(text):
            bad.append(f"{rel}: 87/88-char base58 string (possible secret key)")
    paths = set(filter(None, files.decode().split("\0")))
    paths |= {p.relative_to(ROOT).as_posix() for p in (ROOT / "data").rglob("*") if p.is_file()}
    for rel in sorted(paths):
        path = ROOT / rel
        if not path.is_file() or rel.startswith("secrets/"):
            continue
        raw = path.read_bytes()
        if path.suffix == ".gz":
            try:
                raw = gzip.decompress(raw)
            except OSError:
                continue
        text = raw.decode("utf-8", errors="replace")
        if any(m.group(1) != "REDACTED" for m in API_KEY.finditer(text)):
            bad.append(f"{rel}: provider API key in a URL parameter")
    for b in bad:
        print("SECRET-CHECK FAIL:", b, file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
