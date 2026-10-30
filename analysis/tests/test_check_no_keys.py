"""scripts/check_no_keys.py: a long base58 string fails only when it is a keypair (post-tag change, LOGBOOK)."""
import importlib.util
import os
import pathlib

ROOT = pathlib.Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("check_no_keys", ROOT / "scripts" / "check_no_keys.py")
cnk = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cnk)

# a public transaction signature (data/manifest.jsonl, session 5): 88 chars, not a keypair
SIGNATURE = "5XKUFr8E8bsM84EqGXvaXhkVKigfwy86b16db7LEWZywsfKiKExFCRSMmLWLzViVGebRuCdVFi2eoamfNtPqPpqT"


def _b58encode(b: bytes) -> str:
    n = int.from_bytes(b, "big")
    out = ""
    while n:
        n, r = divmod(n, 58)
        out = cnk._B58[r] + out
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + out


def test_ed25519_rfc8032_vector():
    seed = bytes.fromhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
    assert ed25519_hex(seed).startswith("d75a980182b10ab7d54bfed3c964073a")


def ed25519_hex(seed: bytes) -> str:
    return cnk.ed25519_public(seed).hex()


def test_throwaway_keypair_fails():
    seed = os.urandom(32)
    kp = _b58encode(seed + cnk.ed25519_public(seed))
    assert len(kp) in (87, 88)
    assert cnk.is_keypair_b58(kp)
    assert cnk.has_keypair_b58(f"some text {kp} more text")


def test_real_signature_passes():
    assert len(SIGNATURE) == 88
    assert not cnk.is_keypair_b58(SIGNATURE)
    assert not cnk.has_keypair_b58(f"signature {SIGNATURE} ok")


def test_random_64_bytes_pass():
    for _ in range(3):
        assert not cnk.is_keypair_b58(_b58encode(os.urandom(64)))
