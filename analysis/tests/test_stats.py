import math

import numpy as np
import pytest
from hypothesis import given, settings
from hypothesis import strategies as st
from scipy import stats as sps

from slwlab import power, stats


@settings(max_examples=200, deadline=None)
@given(st.lists(st.integers(0, 4), min_size=3, max_size=10))
def test_tau_b_matches_scipy(xs):
    ys = list(np.random.default_rng(len(xs)).permutation(len(xs)))
    ours = stats.kendall_tau_b(xs, ys)
    ref = sps.kendalltau(xs, ys).statistic
    if math.isnan(ref):
        assert math.isnan(ours)
    else:
        assert ours == pytest.approx(ref, abs=1e-12)


def test_null_is_exact_and_symmetric():
    nt = stats.null_taus([8, 7, 6, 5, 4, 3, 2, 1])
    assert len(nt) == math.factorial(8)
    assert nt.mean() == pytest.approx(0, abs=1e-12)
    assert nt.max() == pytest.approx(1) and nt.min() == pytest.approx(-1)
    # perfect agreement: p = 1/8!
    assert stats.window_pvalue([8, 7, 6, 5, 4, 3, 2, 1], 1.0) == pytest.approx(1 / 40320)


@pytest.mark.parametrize("score", [
    [8, 7, 6, 5, 4, 3, 2, 1], [1, 1, 2, 2, 3, 3, 4, 4], [5, 5, 5, 1, 2, 3], [3, 3, 1, 2, 2, 7, 7],
])
def test_closed_form_null_variance_matches_enumeration(score):
    assert power.null_var_tau(score) == pytest.approx(float(np.var(stats.null_taus(score))), rel=1e-9)


def test_holm():
    adj = stats.holm({"a": 0.01, "b": 0.04, "c": 0.03})
    assert adj["a"] == pytest.approx(0.03)
    assert adj["c"] == pytest.approx(0.06)
    assert adj["b"] == pytest.approx(0.06)  # monotone


def test_randomization_and_normal_approximation_agree():
    rng = np.random.default_rng(5)
    scores, taus, var = [], [], []
    for _ in range(30):
        s = [float(x) for x in rng.permutation(8)]
        pos = list(rng.permutation(8))
        scores.append(s)
        taus.append(stats.tau_vs_positions(s, pos) + 0.08)  # a weak positive effect
        var.append(power.null_var_tau(s))
    p_exact = stats.randomization_pvalue(scores, taus, rng, draws=40_000)
    z = sum(taus) / math.sqrt(sum(var))
    p_norm = 0.5 * math.erfc(z / math.sqrt(2))
    assert p_exact == pytest.approx(p_norm, abs=0.02)


def test_bootstrap_ci_covers_mean():
    rng = np.random.default_rng(1)
    v = rng.normal(0.4, 0.3, 40)
    lo, hi = stats.bootstrap_ci(v, rng, 4000)
    assert lo < v.mean() < hi and hi - lo < 0.3


def test_verdict_levels():
    th = {"alpha": 0.05, "window_tau_norm": 0.5, "strict_mean": 0.8, "strict_ci_lower": 0.6,
          "strict_window_share": 0.6}
    assert stats.verdict([1.0] * 30, 1e-6, (0.9, 1.0), th, True) == "STRICT_RULE"
    assert stats.verdict([0.3] * 30, 1e-3, (0.2, 0.4), th, True) == "SOFT_RULE"
    assert stats.verdict([0.0] * 30, 0.5, (-0.1, 0.1), th, True) == "NO_RULE"
    assert stats.verdict([0.0] * 30, 0.5, (-0.1, 0.1), th, False) == "INCONCLUSIVE"
    # not identifiable in this cell: never STRICT, even when perfect
    assert stats.verdict([1.0] * 30, 1e-6, (0.9, 1.0), None, True) == "SOFT_RULE"
