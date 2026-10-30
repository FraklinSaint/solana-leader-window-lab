#!/usr/bin/env python3
"""Full-history secret scan, run before every push: `python scripts/scan_history.py . <memory dir>`.

Scans every blob reachable from any ref (.gz decompressed), the working tree (tracked and
untracked, not ignored) and every commit message for:
the real provider key(s) found in the local .env/.env.devnet (exact, or 3+ six-char
fragments), any `api-key=` with a value, REAL ed25519 keypair arrays (a 64-number array is a
keypair only if its last 32 bytes equal the public key derived from its first 32 bytes; other
64-number arrays are counted and ignored), 87/88-char base58 strings outside data/ that decode to a keypair (last 32 bytes = public key of the first 32, as scripts/check_no_keys.py; signatures pass), our own
secret keys from secrets/ (64 bytes raw, 32-byte seed raw / hex / base58, 64-byte base58, or
as a number array), and forbidden paths ever committed (.env*, secrets/*, id.json,
*.keypair.json). Also checks the assistant memory directory.
Prints counts and paths only. Never prints a secret value. Exit code 1 on any finding.
"""
import gzip
import hashlib
import json
import pathlib
import re
import subprocess
import sys

# ---- ed25519 public key from a 32-byte seed (RFC 8032, pure Python) ----
_P = 2 ** 255 - 19
_D = -121665 * pow(121666, _P - 2, _P) % _P
_L = 2 ** 252 + 27742317777372353535851937790883648493
_I = pow(2, (_P - 1) // 4, _P)


def _xrecover(y):
    xx = (y * y - 1) * pow(_D * y * y + 1, _P - 2, _P)
    x = pow(xx, (_P + 3) // 8, _P)
    if (x * x - xx) % _P != 0:
        x = x * _I % _P
    return _P - x if x % 2 else x


_BY = 4 * pow(5, _P - 2, _P) % _P
_B = (_xrecover(_BY), _BY, 1, _xrecover(_BY) * _BY % _P)  # extended coordinates


def _add(p, q):
    a = (p[1] - p[0]) * (q[1] - q[0]) % _P
    b = (p[1] + p[0]) * (q[1] + q[0]) % _P
    c = 2 * p[3] * q[3] * _D % _P
    d = 2 * p[2] * q[2] % _P
    e, f, g, h = b - a, d - c, d + c, b + a
    return (e * f % _P, g * h % _P, f * g % _P, e * h % _P)


def _mul(s, p):
    r = (0, 1, 1, 0)
    while s:
        if s & 1:
            r = _add(r, p)
        p = _add(p, p)
        s >>= 1
    return r


def ed25519_public(seed):
    h = hashlib.sha512(seed).digest()
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8
    a |= 1 << 254
    x, y, z, _ = _mul(a, _B)
    zi = pow(z, _P - 2, _P)
    x, y = x * zi % _P, y * zi % _P
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


_cache = {}


def is_real_keypair(vals):
    """True only if vals is 64 bytes whose last 32 equal the public key of its first 32."""
    if len(vals) != 64 or not all(isinstance(v, int) and 0 <= v <= 255 for v in vals):
        return False
    b = bytes(vals)
    if b not in _cache:
        _cache[b] = ed25519_public(b[:32]) == b[32:]
    return _cache[b]


_ALPHA = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58decode(s):
    n = 0
    for c in s:
        n = n * 58 + _ALPHA.index(c)
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return b"\x00" * (len(s) - len(s.lstrip("1"))) + raw


def b58(b):
    n = int.from_bytes(b, "big")
    out = ""
    while n:
        n, r = divmod(n, 58)
        out = _ALPHA[r] + out
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + out


API = re.compile(r"(?i)(?:x-)?api[-_]?key=([A-Za-z0-9_-]{8,})(?![A-Za-z0-9_-])")
ARRAY = re.compile(r"\[\s*(?:\d{1,3}\s*,\s*){63}\d{1,3}\s*\]")
B58 = re.compile(r"(?<![1-9A-HJ-NP-Za-km-z])[1-9A-HJ-NP-Za-km-z]{87,88}(?![1-9A-HJ-NP-Za-km-z])")
BAD_PATH = re.compile(r"(^|/)(\.env(\..*)?|id\.json|[^/]*\.keypair\.json)$")


def main():
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
    own_bin, own_txt = [], []  # byte forms, text forms of our own secret keys
    for sk in local_secret_arrays:
        own_bin += [sk, sk[:32]]
        own_txt += [sk[:32].hex(), b58(sk), b58(sk[:32])]
    own_arrays = set(local_secret_arrays)

    stats = {"arrays": 0, "rejected": 0, "real": 0, "own": 0}
    wstats = {"arrays": 0, "rejected": 0, "real": 0, "own": 0}

    def scan_content(raw, label, findings, stats):
        text = raw.decode("utf-8", "replace")
        if any(k in text for k in env_keys):
            findings.append((label, "REAL Helius key"))
        elif frags and sum(f in text for f in frags) >= 3:
            findings.append((label, "fragments of the Helius key"))
        if any(m.group(1) != "REDACTED" for m in API.finditer(text)):
            findings.append((label, "api-key= with a value"))
        for m in ARRAY.finditer(text):
            vals = json.loads(m.group(0))
            if not all(0 <= v <= 255 for v in vals):
                continue
            stats["arrays"] += 1
            if bytes(vals) in own_arrays:
                stats["own"] += 1
                findings.append((label, "one of OUR secret keys as a number array"))
            if is_real_keypair(vals):
                stats["real"] += 1
                findings.append((label, "real ed25519 keypair (last 32 bytes = pubkey of first 32)"))
            else:
                stats["rejected"] += 1
        if any(b in raw for b in own_bin) or any(t in text for t in own_txt):
            stats["own"] += 1
            findings.append((label, "bytes/hex/base58 of one of OUR secret keys"))

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
        p0 = sorted(paths)[0]
        scan_content(raw, p0, findings, stats)
        if not all(p.startswith("data/") for p in paths) and any(
                is_real_keypair(list(b58decode(m.group(0)))) for m in B58.finditer(raw.decode("utf-8", "replace"))):
            findings.append((p0, "87/88-char base58 string outside data/ that decodes to a keypair"))

    # working tree: tracked and untracked files that are not ignored (secrets/ and .env are ignored)
    wt = [x for x in git("ls-files", "-co", "--exclude-standard", "-z").split("\0") if x]
    wt_scanned = 0
    for rel in wt:
        f = repo / rel
        try:
            if not f.is_file() or f.stat().st_size > 64 * 1024 * 1024:
                continue
            raw = f.read_bytes()
        except OSError:
            continue
        if rel.endswith(".gz"):
            try:
                raw = gzip.decompress(raw)
            except OSError:
                pass
        scan_content(raw, "worktree:" + rel, findings, wstats)
        wt_scanned += 1

    msgs = git("log", "--all", "--format=%B")
    msg_hits = (any(k in msgs for k in env_keys), bool(API.search(msgs)))

    mem_hits = []
    for p in memdir.glob("*.md"):
        t = p.read_text(encoding="utf-8")
        if any(k in t for k in env_keys) or sum(f in t for f in frags) >= 1 or API.search(t):
            mem_hits.append(p.name)

    findings = sorted(set(findings))
    print(f"commits scanned: {len(commits)}; unique blobs: {len(blobs)}; working-tree files: {wt_scanned}; "
          f"key patterns checked: {len(env_keys)} real key(s), {len(local_secret_arrays)} local secret keypair(s)")
    print("forbidden paths ever committed:", sorted(bad_paths) or "none")
    for name, st in (("history", stats), ("working tree", wstats)):
        print(f"64-number arrays in {name}: {st['arrays']} seen, {st['rejected']} not keypairs "
              f"(pubkey check), {st['real']} real keypairs; own-key matches: {st['own']}")
    print("content findings:", findings or "none")
    print("commit messages: real key", msg_hits[0], "| api-key= value", msg_hits[1])
    print("memory files with key/fragments/api-key=:", mem_hits or "none",
          f"({len(list(memdir.glob('*.md')))} files; memory is outside the repo and never pushed)")
    ok = not (bad_paths or findings or any(msg_hits) or mem_hits)
    print("RESULT:", "CLEAN" if ok else "FINDINGS")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
