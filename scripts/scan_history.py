#!/usr/bin/env python3
"""Full-history secret scan, run before every push: `python scripts/scan_history.py . <memory dir>`.

Scans every blob reachable from any ref (.gz decompressed) and every commit message for:
the real provider key(s) found in the local .env/.env.devnet (exact, or 3+ six-char
fragments), any `api-key=` with a value, 64-byte keypair arrays, 87/88-char base58 strings
outside data/, raw bytes of local secret keypairs, and forbidden paths ever committed
(.env*, secrets/*, id.json, *.keypair.json). Also checks the assistant memory directory.
Prints counts and paths only. Never prints a secret value. Exit code 1 on any finding.
"""
import gzip
import json
import pathlib
import re
import subprocess
import sys

repo = pathlib.Path(sys.argv[1])
memdir = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else pathlib.Path("/nonexistent")


def git(*a, binary=False):
    r = subprocess.run(["git", *a], cwd=repo, capture_output=True, check=True)
    return r.stdout if binary else r.stdout.decode("utf-8", "replace")


# secrets to look for, read locally (never printed)
env_keys = set()
for f in [repo / ".env", repo / ".env.devnet"]:
    if f.exists():
        env_keys |= set(re.findall(r"api-key=([A-Za-z0-9_-]+)", f.read_text(encoding="utf-8-sig")))
frags = {k[i:i + 6] for k in env_keys for i in range(0, len(k) - 5)}
local_secret_arrays = []
for p in list((repo / "secrets").rglob("*.json")):
    try:
        arr = json.loads(p.read_text())
        if isinstance(arr, list) and len(arr) == 64:
            local_secret_arrays.append(bytes(arr))
    except Exception:
        pass

API = re.compile(r"(?i)(?:x-)?api[-_]?key=([A-Za-z0-9_-]{8,})(?![A-Za-z0-9_-])")
ARRAY = re.compile(r"\[\s*(?:\d{1,3}\s*,\s*){63}\d{1,3}\s*\]")
B58 = re.compile(r"(?<![1-9A-HJ-NP-Za-km-z])[1-9A-HJ-NP-Za-km-z]{87,88}(?![1-9A-HJ-NP-Za-km-z])")
BAD_PATH = re.compile(r"(^|/)(\.env(\..*)?|id\.json|[^/]*\.keypair\.json)$")

commits = git("rev-list", "--all").split()
blobs = {}  # sha -> set(paths)
bad_paths = set()
for c in commits:
    for line in git("ls-tree", "-r", c).splitlines():
        meta, path = line.split("\t", 1)
        _, typ, sha = meta.split()
        if typ != "blob":
            continue
        blobs.setdefault(sha, set()).add(path)
        if (BAD_PATH.search(path) and path != ".env.example") or (
                path.startswith("secrets/") and path not in ("secrets/README.md", "secrets/.gitkeep")):
            bad_paths.add(path)

findings = []
for sha, paths in blobs.items():
    raw = git("cat-file", "-p", sha, binary=True)
    if any(p.endswith(".gz") for p in paths):
        try:
            raw = gzip.decompress(raw)
        except OSError:
            pass
    text = raw.decode("utf-8", "replace")
    p0 = sorted(paths)[0]
    if any(k in text for k in env_keys):
        findings.append((p0, "REAL Helius key"))
    elif frags and sum(f in text for f in frags) >= 3:
        findings.append((p0, "fragments of the Helius key"))
    if any(m.group(1) != "REDACTED" for m in API.finditer(text)):
        findings.append((p0, "api-key= with a value"))
    for m in ARRAY.finditer(text):
        vals = json.loads(m.group(0))
        if all(0 <= v <= 255 for v in vals):
            findings.append((p0, "64-byte integer array (keypair format)"))
            break
    if not all(p.startswith("data/") for p in paths) and B58.search(text):
        findings.append((p0, "87/88-char base58 string outside data/"))
    for sk in local_secret_arrays:
        if sk in raw:
            findings.append((p0, "raw bytes of a local secret key"))

msgs = git("log", "--all", "--format=%B")
msg_hits = (any(k in msgs for k in env_keys), bool(API.search(msgs)))

mem_hits = []
for p in memdir.glob("*.md"):
    t = p.read_text(encoding="utf-8")
    if any(k in t for k in env_keys) or sum(f in t for f in frags) >= 1 or API.search(t):
        mem_hits.append(p.name)

print(f"commits scanned: {len(commits)}; unique blobs: {len(blobs)}; key patterns checked: "
      f"{len(env_keys)} real key(s), {len(local_secret_arrays)} local secret keypair(s)")
print("forbidden paths ever committed:", sorted(bad_paths) or "none")
print("content findings:", findings or "none")
print("commit messages: real key", msg_hits[0], "| api-key= value", msg_hits[1])
print("memory files with key/fragments/api-key=:", mem_hits or "none",
      f"({len(list(memdir.glob('*.md')))} files; memory is outside the repo and never pushed)")
print("RESULT:", "CLEAN" if not (bad_paths or findings or any(msg_hits) or mem_hits) else "FINDINGS")
sys.exit(0 if not (bad_paths or findings or any(msg_hits) or mem_hits) else 1)
