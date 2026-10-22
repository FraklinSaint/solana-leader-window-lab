"""Statistics of PREREGISTRATION §6–§7.

Unit of analysis = one window. Per window: Kendall tau-b between the observed block order
of our landed txs and a rule's score. The exact null distribution of that tau comes from
all permutations of the landed txs (n <= 8). Across windows:
- estimate = mean tau (or tau_norm), with a window-level percentile bootstrap CI;
- test = randomization test of the sum of window taus, each window drawn from its own exact
  null. One-sided: H1 = "the rule predicts the order", tau > 0;
- Holm correction across the rules tested on the same validator x arm.
"""

from __future__ import annotations

import itertools
import math
from collections.abc import Sequence
from functools import lru_cache

import numpy as np

EXACT_MAX_N = 8


def _sign(v: float) -> int:
    return int(v > 0) - int(v < 0)


def kendall_tau_b(x: Sequence[float], y: Sequence[float]) -> float:
    """Kendall tau-b; NaN when either side is all ties."""
    n = len(x)
    conc = disc = tx = ty = 0
    for i in range(n):
        for j in range(i + 1, n):
            sx, sy = _sign(x[i] - x[j]), _sign(y[i] - y[j])
            if sx == 0 and sy == 0:
                continue
            if sx == 0:
                tx += 1
            elif sy == 0:
                ty += 1
            elif sx == sy:
                conc += 1
            else:
                disc += 1
    denom = math.sqrt((conc + disc + tx) * (conc + disc + ty))
    return (conc - disc) / denom if denom > 0 else float("nan")


def tau_vs_positions(score: Sequence[float], positions: Sequence[int]) -> float:
    """tau-b between a rule's score (high = early) and observed block positions."""
    return kendall_tau_b(list(score), [-p for p in positions])


@lru_cache(maxsize=16)
def _perm_pair_signs(n: int) -> np.ndarray:
    """(n!, n(n-1)/2) array: for each permutation of positions, sign of -(pos_i - pos_j)."""
    pairs = list(itertools.combinations(range(n), 2))
    perms = np.array(list(itertools.permutations(range(n))), dtype=np.int8)
    i, j = np.array(pairs).T
    return -np.sign(perms[:, i] - perms[:, j]).astype(np.int8)


def null_taus(score: Sequence[float], rng: np.random.Generator | None = None, mc: int = 100_000) -> np.ndarray:
    """All tau-b values of `score` against every possible order (exact for n <= 8, else a
    Monte Carlo sample of `mc` random orders). The distribution depends only on the tie
    structure of `score`, so it is cached on that (the multiset of tie-group sizes)."""
    from collections import Counter

    groups = tuple(sorted(Counter(float(x) for x in score).values()))
    return _null_for_groups(groups, mc)


@lru_cache(maxsize=256)
def _null_for_groups(groups: tuple[int, ...], mc: int) -> np.ndarray:
    # A canonical score with this tie structure: group k gets value k.
    s = np.array([float(k) for k, g in enumerate(groups) for _ in range(g)])
    n = len(s)
    pairs = list(itertools.combinations(range(n), 2))
    i, j = np.array(pairs).T
    ss = np.sign(s[i] - s[j])
    n_tied_x = int(np.sum(ss == 0))
    denom = math.sqrt((len(pairs) - n_tied_x) * len(pairs))
    if denom == 0:
        return np.full(1, np.nan)
    if n <= EXACT_MAX_N:
        sp = _perm_pair_signs(n)
    else:
        pos = np.argsort(np.random.default_rng(n).random((mc, n)), axis=1)
        sp = -np.sign(pos[:, i] - pos[:, j])
    out = (sp @ ss) / denom
    out.setflags(write=False)
    return out


def window_pvalue(score: Sequence[float], tau_obs: float) -> float:
    """One-sided exact p-value P(tau >= tau_obs) for a single window."""
    nt = null_taus(score)
    return float(np.mean(nt >= tau_obs - 1e-12))


def bootstrap_ci(values: Sequence[float], rng: np.random.Generator, b: int = 10_000,
                 level: float = 0.95) -> tuple[float, float]:
    v = np.asarray(values, dtype=float)
    v = v[~np.isnan(v)]
    if len(v) == 0:
        return (float("nan"), float("nan"))
    means = v[rng.integers(0, len(v), size=(b, len(v)))].mean(axis=1)
    a = (1 - level) / 2
    return float(np.quantile(means, a)), float(np.quantile(means, 1 - a))


def randomization_pvalue(scores: Sequence[Sequence[float]], taus: Sequence[float],
                         rng: np.random.Generator, draws: int = 20_000) -> float:
    """P(sum of null window taus >= observed sum), each window from its own exact null."""
    keep = [(s, t) for s, t in zip(scores, taus) if not math.isnan(t)]
    if not keep:
        return float("nan")
    obs = sum(t for _, t in keep)
    total = np.zeros(draws)
    for s, _ in keep:
        nt = null_taus(s)
        total += nt[rng.integers(0, len(nt), size=draws)]
    return float((np.sum(total >= obs - 1e-9) + 1) / (draws + 1))


def holm(pvals: dict[str, float]) -> dict[str, float]:
    """Holm-Bonferroni adjusted p-values."""
    items = sorted((p, k) for k, p in pvals.items() if not math.isnan(p))
    m = len(items)
    out: dict[str, float] = {k: float("nan") for k in pvals}
    running = 0.0
    for r, (p, k) in enumerate(items):
        running = max(running, min(1.0, (m - r) * p))
        out[k] = running
    return out


def paired_bootstrap(a: Sequence[float], b: Sequence[float], rng: np.random.Generator,
                     n_boot: int = 10_000) -> tuple[float, tuple[float, float]]:
    """Mean difference a - b over windows where both are defined, with a percentile CI."""
    d = np.asarray(a, float) - np.asarray(b, float)
    d = d[~np.isnan(d)]
    if len(d) == 0:
        return float("nan"), (float("nan"), float("nan"))
    return float(d.mean()), bootstrap_ci(d, rng, n_boot)


def verdict(tau_norm: Sequence[float], p_holm: float, ci: tuple[float, float],
            thresholds: dict | None, power_ok: bool, alpha: float = 0.05) -> str:
    """PREREGISTRATION §7. `thresholds` are the cell's simulator-derived STRICT thresholds,
    or None if the rule is not identifiable in this cell (then it can never be STRICT)."""
    v = np.asarray(tau_norm, float)
    v = v[~np.isnan(v)]
    if len(v) == 0:
        return "INCONCLUSIVE"
    significant = p_holm < alpha
    if significant and thresholds is not None:
        mean = float(v.mean())
        share = float(np.mean(v >= thresholds["window_tau_norm"]))
        if (mean >= thresholds["strict_mean"] and ci[0] >= thresholds["strict_ci_lower"]
                and share >= thresholds["strict_window_share"]):
            return "STRICT_RULE"
    if significant:
        return "SOFT_RULE"
    if power_ok:
        return "NO_RULE"
    return "INCONCLUSIVE"
