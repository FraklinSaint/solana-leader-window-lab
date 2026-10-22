"""model.py / rules.py agree with the Rust implementation (crates/common/src/cost.rs and
crates/sender/src/arms.rs tests use the same hand-computed values)."""

import numpy as np

from slwlab import rules
from slwlab.b58 import b58decode
from slwlab.model import ArmConfig, Tx, design, geometric, priority_fee


def tx(cu_limit=1000, price=0, rank=0, conflict=False, idx=0):
    return Tx(idx=idx, cu_limit=cu_limit, price=price, send_rank=rank, conflict=conflict)


def test_cost_matches_rust_hand_computed():
    # cost.rs::cost_hand_computed: 720 + 300 + 31/4 + 1000 + 8*1 = 2035
    assert tx().cost_units() == 2035
    assert tx().cost_units(None) == 2035 - 8 + 16_384
    assert tx().cost_units(32 * 1024 + 1) == 2035 + 8
    assert tx(conflict=True).cost_units() == 2035 + 300


def test_fees_match_mainnet_get_fee_for_message():
    # fees.rs::matches_mainnet_get_fee_for_message
    for price, prio in [(0, 0), (1, 1), (999, 1), (1000, 1), (1001, 2), (1_000_000, 1000)]:
        assert priority_fee(price, 1000) == prio


def test_agave_priority_formula():
    t = tx(price=1_000_000)
    reward = 1000 + 5000 - 2500
    assert t.agave_reward() == reward
    assert t.agave_priority() == reward * 1_000_000 // (2035 + 1)


def test_fd_ratio_equals_integer_cross_multiplication():
    rng = np.random.default_rng(3)
    for _ in range(500):
        a = tx(cu_limit=int(rng.integers(500, 20_000)), price=int(rng.integers(0, 2_000_000)))
        b = tx(cu_limit=int(rng.integers(500, 20_000)), price=int(rng.integers(0, 2_000_000)))
        lhs = a.fd_rewards * b.cost_units()
        rhs = b.fd_rewards * a.cost_units()
        fa, fb = rules.r_ratio_fd([a, b])
        assert (fa > fb) == (lhs > rhs) and (fa == fb) == (lhs == rhs)


def test_geometric_ladder_is_the_rust_one():
    v = geometric(10_000, 1_000_000, 8)
    assert v[0] == 10_000 and v[-1] == 1_000_000
    assert all(x < y for x, y in zip(v, v[1:]))


def _order(score):
    return sorted(range(len(score)), key=lambda i: -score[i])


def test_arm_p_fee_rules_agree_like_rust():
    d = design(ArmConfig(), "P", np.random.default_rng(42))
    p = _order(rules.r_price(d.txs))
    assert p == _order(rules.r_total(d.txs)) == _order(rules.r_ratio_agave(d.txs)) == _order(rules.r_ratio_fd(d.txs))
    assert p == list(range(len(d.txs) - 1, -1, -1))


def test_arm_r_total_and_ratio_are_reversed_like_rust():
    d = design(ArmConfig(), "R", np.random.default_rng(42))
    total = _order(rules.r_total(d.txs))
    ratio = _order(rules.r_ratio_agave(d.txs))
    assert total == list(range(len(d.txs) - 1, -1, -1))
    assert ratio == list(range(len(d.txs)))
    assert ratio == _order(rules.r_ratio_fd(d.txs))
    assert len(set(rules.r_price(d.txs))) == 1  # R_PRICE: all ties in Arm R


def test_arm_x_price_against_send_order():
    d = design(ArmConfig(), "X", np.random.default_rng(1), x_gap_ms=25)
    assert [t.send_rank for t in d.txs] == list(range(len(d.txs)))
    assert _order(rules.r_price(d.txs)) == list(range(len(d.txs) - 1, -1, -1))
    assert _order(rules.r_fifo(d.txs)) == list(range(len(d.txs)))


def test_fba_groups_by_buffer_then_fee():
    txs = [tx(price=p, idx=i, rank=i) for i, p in enumerate([10_000, 50_000, 20_000, 90_000])]
    arrival_us = [0, 10_000, 60_000, 70_000]  # buffers: {0,1}, {2,3} with T=50 ms
    s = rules.make_fba(50)(txs, arrival_us)
    assert _order(s) == [1, 0, 3, 2]


def test_tie_rules_compare_raw_bytes_not_base58_text():
    # "2" < "z" as text, but as bytes 'z'(57) > '2'(1); and leading '1's are zero bytes.
    keys = ["11112", "1111z", "2111z"]
    raw = [b58decode(k) for k in keys]
    assert raw[0] < raw[1] < raw[2]
    txs = [Tx(idx=i, cu_limit=1000, price=1, send_rank=i, signature=k) for i, k in enumerate(keys)]
    assert _order(rules.tie_signature(txs)) == [0, 1, 2]
