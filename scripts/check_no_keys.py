#!/usr/bin/env python3
"""Fail if any tracked file looks like it contains a Solana secret key.

Checks every `git ls-files` path for:
  * a JSON array of exactly 64 integers in 0..255 (solana-keygen keypair format)
  * an 87/88-char base58 string that decodes to a Solana keypair: 64 bytes whose last 32 bytes are
    the ed25519 public key of the first 32 (a transaction signature never has that property)
  * anything tracked under secrets/ other than README.md and .gitkeep
  * a provider API key in a URL parameter (api-key=..., api_key=..., apikey=...), in any
    tracked file and in every file under data/ (tracked or not; .gz decompressed)
"""
import gzip
import hashlib
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
ARRAY = re.compile(r"\[\s*(?:\d{1,3}\s*,\s*){63}\d{1,3}\s*\]")
B58_SECRET = re.compile(r"(?<![1-9A-HJ-NP-Za-km-z])[1-9A-HJ-NP-Za-km-z]{87,88}(?![1-9A-HJ-NP-Za-km-z])")
_B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
_P = 2**255 - 19
_D = -121665 * pow(121666, _P - 2, _P) % _P


def _b58decode(s: str) -> bytes:
    n = 0
    for c in s:
        n = n * 58 + _B58.index(c)
    b = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return b"\x00" * (len(s) - len(s.lstrip("1"))) + b


def _edwards_add(a, b):
    x1, y1, z1, t1 = a
    x2, y2, z2, t2 = b
    p = _P
    aa, bb = (y1 - x1) * (y2 - x2) % p, (y1 + x1) * (y2 + x2) % p
    c, d = t1 * 2 * _D * t2 % p, z1 * 2 * z2 % p
    e, f, g, h = bb - aa, d - c, d + c, bb + aa
    return (e * f % p, g * h % p, f * g % p, e * h % p)


def _base_point():
    p = _P
    y = 4 * pow(5, p - 2, p) % p
    x2 = (y * y - 1) * pow(_D * y * y + 1, p - 2, p) % p
    x = pow(x2, (p + 3) // 8, p)
    if (x * x - x2) % p:
        x = x * pow(2, (p - 1) // 4, p) % p
    if x & 1:
        x = p - x
    return (x, y, 1, x * y % p)


def ed25519_public(seed: bytes) -> bytes:
    """Public key of a 32-byte ed25519 seed (RFC 8032, stdlib only; used for the keypair test only)."""
    h = bytearray(hashlib.sha512(seed).digest()[:32])
    h[0] &= 248
    h[31] &= 127
    h[31] |= 64
    k, q, r = int.from_bytes(h, "little"), _base_point(), (0, 1, 1, 0)
    while k:
        if k & 1:
            r = _edwards_add(r, q)
        q = _edwards_add(q, q)
        k >>= 1
    zi = pow(r[2], _P - 2, _P)
    x, y = r[0] * zi % _P, r[1] * zi % _P
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


def is_keypair_b58(s: str) -> bool:
    """True if `s` is the base58 of a 64-byte Solana keypair (last 32 bytes = public key of the first 32)."""
    try:
        b = _b58decode(s)
    except ValueError:
        return False
    return len(b) == 64 and ed25519_public(b[:32]) == b[32:]


def has_keypair_b58(text: str) -> bool:
    return any(is_keypair_b58(m.group(0)) for m in B58_SECRET.finditer(text))


# Same rule as crates/sender/src/redact.rs::contains_api_key.
API_KEY = re.compile(r"(?i)(?:x-)?api[-_]?key=([A-Za-z0-9_-]{8,})(?![A-Za-z0-9_-])")
ALLOWED_IN_SECRETS = {"secrets/README.md", "secrets/.gitkeep"}
# Raw blocks and the documents contain 87/88-char base58 *signatures*; those are public and expected.
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
        if not rel.startswith(B58_EXEMPT_PREFIXES) and has_keypair_b58(text):
            bad.append(f"{rel}: 87/88-char base58 string that is a keypair (secret key)")
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
