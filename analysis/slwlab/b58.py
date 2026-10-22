"""Base58 (Bitcoin alphabet) decoding, stdlib only."""

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
_IDX = {c: i for i, c in enumerate(B58)}


def b58decode(s: str) -> bytes:
    n = 0
    for c in s:
        n = n * 58 + _IDX[c]
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    return b"\0" * (len(s) - len(s.lstrip("1"))) + raw
