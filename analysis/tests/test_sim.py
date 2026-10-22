"""Sanity properties of the SIMULATION models (not measurements)."""

import numpy as np

from slwlab import rules, sim, stats
from slwlab.model import ArmConfig


def mean_tau(arm, sch, nz, rule, k=300, seed=0):
    rng = np.random.default_rng(seed)
    out = []
    for _ in range(k):
        w = sim.simulate_window(arm, sch, nz, rng, ArmConfig())
        if len(w.landed) < 5:
            continue
        txs = [w.design.txs[i] for i in w.landed]
        arr = [w.arrival_us[i] for i in w.landed]
        out.append(stats.tau_vs_positions(rules.score(rule, txs, arr), range(len(txs))))
    return float(np.nanmean(out))


def test_null_model_has_no_rule():
    for rule in ["R_PRICE", "R_FIFO"]:
        assert abs(mean_tau("P", sim.RANDOM, sim.MEDIUM, rule)) < 0.06


def test_no_interleaving_means_exact_priority():
    # one worker, no background, everything held until window start: pure priority order
    nz = sim.with_nuisance(sim.LOW, bg_between=0.0, clock_sd_ms=0.0, p_land=1.0)
    one = sim.Scheduler("agave", workers=1, batch_max=64)
    assert mean_tau("P", one, nz, "R_RATIO_AGAVE") == 1.0


def test_threads_degrade_but_keep_the_sign():
    t = mean_tau("P", sim.AGAVE, sim.MEDIUM, "R_RATIO_AGAVE")
    assert 0.2 < t < 0.9


def test_conflict_chain_is_closer_to_strict_than_lock_free():
    assert mean_tau("C", sim.AGAVE, sim.MEDIUM, "R_RATIO_AGAVE") > mean_tau("P", sim.AGAVE, sim.MEDIUM, "R_RATIO_AGAVE") + 0.2


def test_fifo_builder_follows_send_order_in_arm_x():
    assert mean_tau("X", sim.FIFO, sim.MEDIUM, "R_FIFO") > 0.6
    assert mean_tau("X", sim.FIFO, sim.MEDIUM, "R_PRICE") < -0.6


def test_firedancer_ties_go_lifo():
    assert mean_tau("F", sim.FD2, sim.MEDIUM, "TIE_ARRIVAL_LIFO") > 0.3
    assert abs(mean_tau("F", sim.AGAVE, sim.MEDIUM, "TIE_ARRIVAL_LIFO")) < 0.06


def test_arm_f_needs_its_extra_lead_on_agave():
    # Without the extra lead, ~25% of gapped bursts straddle the window start: late txs go
    # to later scheduling passes and Agave's (unobservable) tie-break looks FIFO.
    no_lead = sim.with_nuisance(sim.MEDIUM, f_extra_lead_ms=0.0)
    assert mean_tau("F", sim.AGAVE, no_lead, "TIE_ARRIVAL_FIFO") > 0.15
