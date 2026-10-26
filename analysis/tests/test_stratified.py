"""PREREGISTRATION §8 option B: the pooled test is stratified by independence unit."""
import math

import numpy as np

from slwlab import stats

SCORE = [5.0, 4.0, 3.0, 2.0, 1.0, 0.0]


def _unit(n, tau_positions):
    scores = [SCORE] * n
    taus = [stats.tau_vs_positions(SCORE, tau_positions)] * n
    return scores, taus


def test_one_dominant_unit_cannot_carry_the_pooled_verdict():
    rule_follows = list(range(6))            # tau = +1
    rule_reversed = list(reversed(range(6)))  # tau = -1
    big = _unit(60, rule_follows)
    small = [_unit(6, rule_reversed), _unit(6, rule_reversed)]
    units = [big] + small
    scores = [u[0] for u in units]
    taus = [u[1] for u in units]
    rng = np.random.default_rng(1)
    flat_scores = [s for u in scores for s in u]
    flat_taus = [t for u in taus for t in u]
    p_window_weighted = stats.randomization_pvalue(flat_scores, flat_taus, rng, draws=4000)
    p_stratified = stats.stratified_randomization_pvalue(scores, taus, rng, draws=4000)
    assert p_window_weighted < 0.01  # the old pooled test: the big unit decides
    assert p_stratified > 0.9        # the stratified test: two of three units go the other way
    assert math.isclose(stats.stratified_mean(taus), (1 - 1 - 1) / 3)


def test_all_units_following_the_rule_is_significant():
    units = [_unit(n, list(range(6))) for n in (30, 12, 8)]
    p = stats.stratified_randomization_pvalue([u[0] for u in units], [u[1] for u in units],
                                              np.random.default_rng(2), draws=2000)
    assert p < 0.01


def test_small_units_are_left_out_and_one_unit_is_not_a_pool():
    big = _unit(20, list(range(6)))
    tiny = _unit(stats.MIN_UNIT_WINDOWS - 1, list(range(6)))
    taus = [big[1], tiny[1]]
    assert stats.stratified_units(taus) == [0]
    assert math.isnan(stats.stratified_mean(taus))
    assert math.isnan(stats.stratified_randomization_pvalue([big[0], tiny[0]], taus,
                                                            np.random.default_rng(3), draws=100))
    assert math.isnan(stats.stratified_bootstrap_ci(taus, np.random.default_rng(3), b=100)[0])


def test_type_one_error_under_the_null():
    rng = np.random.default_rng(4)
    rejections, reps = 0, 200
    for _ in range(reps):
        scores, taus = [], []
        for n in (14, 9, 6):
            ws, ts = [], []
            for _ in range(n):
                pos = list(rng.permutation(6))
                ws.append(SCORE)
                ts.append(stats.tau_vs_positions(SCORE, pos))
            scores.append(ws)
            taus.append(ts)
        if stats.stratified_randomization_pvalue(scores, taus, rng, draws=499) < 0.05:
            rejections += 1
    assert 0.01 <= rejections / reps <= 0.11, rejections / reps


def test_stratified_bootstrap_resamples_inside_units():
    a = [1.0] * 10
    b = [-1.0] * 10
    lo, hi = stats.stratified_bootstrap_ci([a, b], np.random.default_rng(5), b=200)
    assert lo == hi == 0.0  # no within-unit variation: the CI is the stratified mean
