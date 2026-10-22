#!/usr/bin/env python3
"""Fail if any tracked file looks like it contains a Solana secret key.

Checks every `git ls-files` path for:
  * a JSON array of exactly 64 integers in 0..255 (solana-keygen keypair format)
  * an 87/88-char base58 string (64-byte secret key encoding)
  * anything tracked under secrets/ other than README.md and .gitkeep
"""
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
ARRAY = re.compile(r"\[\s*(?:\d{1,3}\s*,\s*){63}\d{1,3}\s*\]")
B58_SECRET = re.compile(r"(?<![1-9A-HJ-NP-Za-km-z])[1-9A-HJ-NP-Za-km-z]{87,88}(?![1-9A-HJ-NP-Za-km-z])")
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
    for b in bad:
        print("SECRET-CHECK FAIL:", b, file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
