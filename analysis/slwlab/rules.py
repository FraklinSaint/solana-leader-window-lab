"""Candidate ordering rules (PREREGISTRATION §2, §4).

Every rule maps the window's transactions to a *score*: the higher the score, the earlier
the rule says the tx is placed in the block. Equal scores are ties, and the rule makes no
claim about their order. Kendall tau-b (stats.py) handles ties on both sides. A rule never
looks at the observed order.
"""

from __future__ import annotations

from collections.abc import Callable, Sequence

from .model import Tx

Score = list[float]
Rule = Callable[[Sequence[Tx], Sequence[float] | None], Score]


def r_price(txs, arrival=None) -> Score:
    return [float(t.price) for t in txs]


def r_total(txs, arrival=None) -> Score:
    return [float(t.priority_fee) for t in txs]


def r_ratio_agave(txs, arrival=None) -> Score:
    return [float(t.agave_priority()) for t in txs]


def r_ratio_fd(txs, arrival=None) -> Score:
    # Exact rational comparison in fd_pack (COMPARE_WORSE). A float ratio is exact enough to
    # order our values, and tests check it against the integer cross-multiplication.
    return [t.fd_rewards / t.cost_units() for t in txs]


def _times(txs, arrival):
    """Arrival times if known (actual send times), else the planned send offsets."""
    if arrival is not None:
        return list(arrival)
    return [t.send_offset_us + t.send_rank * 1e-3 for t in txs]


def r_fifo(txs, arrival=None) -> Score:
    # Planned offsets can tie (gap 0): send rank breaks them, as the sender does.
    if arrival is None:
        return [-float(t.send_rank) for t in txs]
    return [-float(a) for a in arrival]


def make_fba(t_ms: float) -> Rule:
    """FBA(T): arrival buffers of T ms, FIFO across buffers, highest priority fee first
    within a buffer (Harmonic docs, VERIFIED_FACTS F7). The buffer phase is unknown. We
    align buffers to the first arrival, the most favourable phase for keeping a burst in
    one buffer, and the simulator checks sensitivity to the phase."""

    def rule(txs, arrival=None) -> Score:
        times = _times(txs, arrival)
        t0 = min(times)
        width = t_ms * 1000.0
        big = 1e12
        return [-(int((tm - t0) // width)) * big + float(t.priority_fee) for t, tm in zip(txs, times)]

    rule.__name__ = f"r_fba_{int(t_ms)}"
    return rule


def r_cost(txs, arrival=None) -> Score:
    """Cheapest-first; not a hypothesis, a diagnostic for Arm R."""
    return [-float(t.cost_units()) for t in txs]


RULES: dict[str, Rule] = {
    "R_PRICE": r_price,
    "R_TOTAL": r_total,
    "R_RATIO_AGAVE": r_ratio_agave,
    "R_RATIO_FD": r_ratio_fd,
    "R_FIFO": r_fifo,
    "R_FBA50": make_fba(50),
}

# ------------------------------------------------------------------ Arm F tie-breaks (§4)


def tie_arrival_fifo(txs, arrival=None) -> Score:
    return r_fifo(txs, arrival)


def tie_arrival_lifo(txs, arrival=None) -> Score:
    return [-s for s in r_fifo(txs, arrival)]


def _bytes_desc_score(keys: Sequence[str]) -> Score:
    """Ascending key bytes -> earlier. Keys are base58 strings, so decode them to compare
    the raw bytes (base58 text order is not byte order)."""
    from .b58 import b58decode

    raw = [b58decode(k) for k in keys]
    order = sorted(range(len(raw)), key=lambda i: raw[i])
    score = [0.0] * len(raw)
    for rank, i in enumerate(order):
        score[i] = float(len(raw) - rank)
    return score


def tie_signature(txs, arrival=None) -> Score:
    return _bytes_desc_score([t.signature for t in txs])


def tie_payer(txs, arrival=None) -> Score:
    return _bytes_desc_score([t.payer for t in txs])


TIE_RULES: dict[str, Rule] = {
    "TIE_ARRIVAL_FIFO": tie_arrival_fifo,
    "TIE_ARRIVAL_LIFO": tie_arrival_lifo,
    "TIE_SIGNATURE": tie_signature,
    "TIE_PAYER": tie_payer,
}

# Which rules each arm can discriminate (others are all-ties there and carry no information).
RULES_BY_ARM: dict[str, list[str]] = {
    "P": ["R_PRICE", "R_TOTAL", "R_RATIO_AGAVE", "R_RATIO_FD", "R_FIFO", "R_FBA50"],
    "C": ["R_PRICE", "R_TOTAL", "R_RATIO_AGAVE", "R_RATIO_FD", "R_FIFO", "R_FBA50"],
    "X": ["R_PRICE", "R_FIFO", "R_FBA50"],
    "R": ["R_TOTAL", "R_RATIO_AGAVE", "R_RATIO_FD", "R_FIFO"],
    "F": ["TIE_ARRIVAL_FIFO", "TIE_ARRIVAL_LIFO", "TIE_SIGNATURE", "TIE_PAYER"],
}


def score(name: str, txs: Sequence[Tx], arrival: Sequence[float] | None = None) -> Score:
    fn = RULES.get(name) or TIE_RULES.get(name)
    if fn is None:
        raise KeyError(name)
    return fn(txs, arrival)
