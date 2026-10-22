"""Cost, reward and priority of our transactions, and the arm designs.

Transcribed from crates/common/src/cost.rs and crates/sender/src/arms.rs, which cite the
pinned sources (VERIFIED_FACTS F5, F6). tests/test_model.py checks the hand-computed values
used by the Rust tests, so the two implementations cannot drift silently.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np

SIGNATURE_COST = 720
WRITE_LOCK_UNITS = 300
INSTRUCTION_DATA_BYTES_PER_CU = 4
HEAP_COST_PER_PAGE = 8
ACCOUNT_DATA_PAGE_SIZE = 32 * 1024
MAX_LOADED_ACCOUNTS_DATA_SIZE_BYTES = 64 * 1024 * 1024
BURN_PERCENT = 50
FD_PACK_FEE_PER_SIGNATURE = 5000
AGAVE_PRIORITY_MULTIPLIER = 1_000_000
MICRO_LAMPORTS_PER_LAMPORT = 1_000_000

# txbuild.rs: SetComputeUnitLimit(5) + SetComputeUnitPrice(9) + SetLoadedAccountsDataSizeLimit(5)
# + system transfer(12) bytes of instruction data; explicit 32 KiB loaded-data limit.
INSTRUCTION_DATA_LEN = 5 + 9 + 5 + 12
LOADED_ACCOUNTS_DATA_LIMIT = 32 * 1024


def priority_fee(cu_price_micro_lamports: int, cu_limit: int) -> int:
    """ceil(price * limit / 1e6) lamports (fees.rs)."""
    return -(-(cu_price_micro_lamports * cu_limit) // MICRO_LAMPORTS_PER_LAMPORT)


@dataclass(frozen=True)
class Tx:
    """One of our transactions in a window, as recorded in the manifest."""

    idx: int
    cu_limit: int
    price: int  # micro-lamports per CU
    send_rank: int
    send_offset_us: int = 0
    conflict: bool = False  # Arm C: one extra writable account
    signature: str = ""
    payer: str = ""

    @property
    def n_writable(self) -> int:
        return 2 if self.conflict else 1

    def cost_units(self, loaded_limit: int | None = LOADED_ACCOUNTS_DATA_LIMIT) -> int:
        loaded = min(loaded_limit or MAX_LOADED_ACCOUNTS_DATA_SIZE_BYTES, MAX_LOADED_ACCOUNTS_DATA_SIZE_BYTES)
        pages = -(-loaded // ACCOUNT_DATA_PAGE_SIZE)
        return (
            SIGNATURE_COST
            + WRITE_LOCK_UNITS * self.n_writable
            + INSTRUCTION_DATA_LEN // INSTRUCTION_DATA_BYTES_PER_CU
            + self.cu_limit
            + HEAP_COST_PER_PAGE * pages
        )

    @property
    def priority_fee(self) -> int:
        return priority_fee(self.price, self.cu_limit)

    def agave_reward(self, lamports_per_signature: int = 5000) -> int:
        sig_fee = lamports_per_signature
        return self.priority_fee + sig_fee - sig_fee * BURN_PERCENT // 100

    def agave_priority(self, lamports_per_signature: int = 5000) -> int:
        return self.agave_reward(lamports_per_signature) * AGAVE_PRIORITY_MULTIPLIER // (self.cost_units() + 1)

    @property
    def fd_rewards(self) -> int:
        return FD_PACK_FEE_PER_SIGNATURE * BURN_PERCENT // 100 + self.priority_fee


# ---------------------------------------------------------------- arm designs (arms.rs)

@dataclass(frozen=True)
class ArmConfig:
    n: int = 6  # pre-registered (docs/SIMULATION.md); arms.rs ArmConfig::default()
    cu_limit: int = 1_000
    price_min: int = 10_000
    price_max: int = 1_000_000
    f_price: int = 50_000
    f_gap_us: int = 10_000
    x_gaps_ms: tuple[int, ...] = (0, 10, 25, 60, 120)
    r_price: int = 200_000
    r_cu_limits: tuple[int, ...] = (1_000, 2_000, 3_000, 4_000, 5_000, 6_000, 7_000, 8_000, 9_000, 10_000)
    p_gap_us: int = 0


def geometric(lo: int, hi: int, n: int) -> list[int]:
    if n == 1:
        return [hi]
    r = (hi / lo) ** (1 / (n - 1))
    return [round(lo * r**i) for i in range(n)]


@dataclass
class Design:
    arm: str
    gap_us: int
    txs: list[Tx] = field(default_factory=list)


def design(cfg: ArmConfig, arm: str, rng: np.random.Generator, x_gap_ms: int = 0) -> Design:
    """Same parameters as arms.rs::design (the shuffle uses numpy instead of ChaCha, which
    only matters for simulation; real windows are read from the manifest)."""
    n = cfg.n
    order = list(range(n))
    if arm in ("P", "C"):
        rng.shuffle(order)
        prices, limits, gap = geometric(cfg.price_min, cfg.price_max, n), [cfg.cu_limit] * n, cfg.p_gap_us
    elif arm == "F":
        prices, limits, gap = [cfg.f_price] * n, [cfg.cu_limit] * n, cfg.f_gap_us
    elif arm == "X":
        prices, limits, gap = geometric(cfg.price_min, cfg.price_max, n), [cfg.cu_limit] * n, x_gap_ms * 1000
    elif arm == "R":
        rng.shuffle(order)
        prices, limits, gap = [cfg.r_price] * n, list(cfg.r_cu_limits[:n]), cfg.p_gap_us
        assert len(limits) == n
    else:
        raise ValueError(arm)
    rank = {idx: r for r, idx in enumerate(order)}
    txs = [
        Tx(idx=i, cu_limit=limits[i], price=prices[i], send_rank=rank[i],
           send_offset_us=rank[i] * gap, conflict=(arm == "C"))
        for i in range(n)
    ]
    return Design(arm=arm, gap_us=gap, txs=txs)
