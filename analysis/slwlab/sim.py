"""SIMULATION: block order of our N transactions under candidate scheduler models.

Everything this module produces is labeled SIMULATION and never mixed with measurements.

Models (sources in VERIFIED_FACTS F6, F6b, F7):

* ``agave``: greedy scheduler, ``DEFAULT_NUM_WORKERS = 4`` (core/src/banking_stage.rs:85).
  Txs that arrive before the window are held, then popped in priority order (R_RATIO_AGAVE)
  at window start. Exact ties pop in an order that cannot be observed from outside
  (highest Slab index), modeled as random. Each pop goes to the least-loaded worker thread.
  Batches are capped at 64 txs, and each batch is recorded when its thread finishes it. Our
  txs compete with a background of other pops, so they spread over threads and batches.
  Block position = batch completion order, then order within the batch. Txs arriving after
  window start are scheduled in the next scheduling pass.
* ``fd``: Firedancer pack, best-first by rewards/cost (R_RATIO_FD), microblocks of at most
  ``MAX_TXN_PER_MICROBLOCK = 5`` txs (src/disco/pack/fd_microblock.h:71). Microblocks go to
  2 exec tiles (firedancer/config/default.toml:1054; Frankendancer: 4 bank tiles,
  fdctl/config/default.toml:834). Exact ties: the last to arrive is scheduled first (F6b).
* Arm C (shared writable account): Agave puts the whole chain on one thread, in pop order.
  Firedancer schedules about one tx per microblock, best first, sequentially. Both are
  strict priority within one scheduling pass.
* ``fifo``: arrival order at the builder (Harmonic ``fifo``).
* ``fba``: 50 ms buffers with random phase; priority fee descending inside a buffer
  (Harmonic ``fba``).
* ``random``: uniform random order (the null).
* ``mix``: each window follows ``base`` with probability ``p_rule``, else random order
  ("soft rule", used to check that STRICT does not fire on partial rules).

Unknown nuisance parameters (background load, execution-time noise, arrival jitter, clock
error, landing rate) are swept over pre-registered grids. The primary setting is ``MEDIUM``.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, replace

import numpy as np

from .model import ArmConfig, Design, Tx, design


@dataclass(frozen=True)
class Nuisance:
    # background pops between two of our pops (Poisson mean)
    bg_between: float = 3.0
    # initial busy time of each worker, uniform [0, busy_max_ms]
    busy_max_ms: float = 2.0
    # execution time per tx in a batch (ms) and lognormal sigma of a batch's time
    exec_ms_per_tx: float = 0.05
    exec_sigma: float = 0.5
    # scheduling pass interval once the window has started (ms)
    pass_ms: float = 1.0
    # per-tx network jitter (ms, normal sd) and burst offset vs window start (ms)
    jitter_ms: float = 1.0
    offset_ms: float = -100.0
    # Arm F is sent this much earlier (ArmConfig.f_extra_lead_ms in arms.rs)
    f_extra_lead_ms: float = 200.0
    clock_sd_ms: float = 100.0
    # probability that a tx lands in the intended slot
    p_land: float = 0.9


LOW = Nuisance(bg_between=0.5, busy_max_ms=0.5, exec_sigma=0.25, jitter_ms=0.3)
MEDIUM = Nuisance()
HIGH = Nuisance(bg_between=10.0, busy_max_ms=5.0, exec_sigma=0.8, jitter_ms=3.0, p_land=0.75)
NUISANCE_GRID = {"low": LOW, "medium": MEDIUM, "high": HIGH}


@dataclass(frozen=True)
class Scheduler:
    kind: str  # agave | fd | fifo | fba | random | mix
    workers: int = 4
    batch_max: int = 64
    fba_ms: float = 50.0
    base: str = ""  # for mix: a SCHEDULERS name ...
    p_rule: float = 1.0
    base_sched: "Scheduler | None" = None  # ... or the base scheduler itself
    # agave/fd only: pop order by this rule (rules.py) instead of the client's own priority,
    # i.e. "rule R + this client's thread interleaving" (tau ceiling of R for the family)
    pop_rule: str = ""


AGAVE = Scheduler("agave", workers=4, batch_max=64)
FD2 = Scheduler("fd", workers=2, batch_max=5)
FD4 = Scheduler("fd", workers=4, batch_max=5)
FIFO = Scheduler("fifo")
FBA = Scheduler("fba")
RANDOM = Scheduler("random")
SCHEDULERS = {"agave": AGAVE, "fd2": FD2, "fd4": FD4, "fifo": FIFO, "fba50": FBA, "random": RANDOM}


def arrivals_ms(d: Design, nz: Nuisance, rng: np.random.Generator) -> np.ndarray:
    """Arrival time of each tx (by idx) at the leader, relative to the window start."""
    lead = nz.f_extra_lead_ms if d.arm == "F" else 0.0
    burst = nz.offset_ms - lead + rng.normal(0, nz.clock_sd_ms)
    send = np.array([t.send_offset_us / 1000.0 for t in d.txs])
    return burst + send + rng.normal(0, nz.jitter_ms, size=len(d.txs))


def _priority(t: Tx, kind: str) -> float:
    return float(t.agave_priority()) if kind == "agave" else t.fd_rewards / t.cost_units()


def _threaded(d: Design, arr: np.ndarray, sch: Scheduler, nz: Nuisance,
              rng: np.random.Generator, out: dict | None = None) -> list[int]:
    """Block order (list of idx) under the agave / fd thread model.

    Each pop (ours or background) goes to the worker that becomes free first and adds a
    lognormal execution time to it. A worker's pops form batches of at most ``batch_max``.
    A batch is recorded when its last member finishes. Our txs are ordered by
    (batch record time, pop order).
    """
    n = len(d.txs)
    conflict = d.arm == "C"
    passes = np.where(arr <= 0, 0.0, np.ceil(arr / nz.pass_ms) * nz.pass_ms)
    if sch.pop_rule:
        from . import rules

        prio = np.array(rules.score(sch.pop_rule, d.txs, list(arr * 1000.0)))
    else:
        prio = np.array([_priority(t, sch.kind) for t in d.txs])
    # Agave: Slab index, unobservable -> random. FD: last arrival first among exact ties.
    tiebreak = rng.random(n) if sch.kind == "agave" else arr
    pop = sorted(range(n), key=lambda i: (passes[i], -prio[i], -tiebreak[i]))
    w = sch.workers
    free = rng.uniform(0, nz.busy_max_ms, size=w)
    fill = np.zeros(w, dtype=int)
    bid = np.zeros(w, dtype=int)
    batch_end: dict[tuple[int, int], float] = {}
    member: dict[int, tuple[int, int]] = {}

    def run_on(th: int, t_pass: float) -> tuple[int, int]:
        if fill[th] >= sch.batch_max or free[th] < t_pass:
            # a full batch, or the worker went idle: the next pop opens a new batch
            if fill[th] > 0:
                bid[th] += 1
            fill[th] = 0
        fill[th] += 1
        free[th] = max(free[th], t_pass) + nz.exec_ms_per_tx * math.exp(rng.normal(0, nz.exec_sigma))
        b = (th, int(bid[th]))
        batch_end[b] = float(free[th])
        return b

    chain_thread = None
    chain_end = 0.0
    bg: list[tuple[int, int]] = []
    for i in pop:
        t_pass = float(passes[i])
        if conflict and sch.kind == "fd":
            # Firedancer: one tx of the chain per microblock, each after the previous
            # finished (the write lock is held until then): strictly sequential.
            th = int(np.argmin(free))
            free[th] = max(free[th], t_pass, chain_end)
            fill[th] = sch.batch_max  # its own microblock
            member[i] = run_on(th, t_pass)
            chain_end = batch_end[member[i]]
            continue
        if not conflict:
            for _ in range(rng.poisson(nz.bg_between)):  # background pops before ours
                bg.append(run_on(int(np.argmin(free)), t_pass))
        if conflict:  # Agave: the chain follows the thread that holds the write lock
            th = chain_thread if chain_thread is not None else int(np.argmin(free))
            chain_thread = th
        else:
            th = int(np.argmin(free))
        member[i] = run_on(th, t_pass)
    rank = {i: k for k, i in enumerate(pop)}
    order = sorted(range(n), key=lambda i: (batch_end[member[i]], rank[i]))
    if out is not None:
        # Block positions of our txs among ours + background pops (order-agnostic span
        # statistic, used to estimate background load from calibration blocks).
        keys = [(batch_end[member[i]], 2 * rank[i], ("ours", i)) for i in range(n)]
        keys += [(batch_end[b], 2 * k + 1, ("bg", k)) for k, b in enumerate(bg)]
        keys.sort(key=lambda x: (x[0], x[1]))
        out["pos"] = {tag[1]: p for p, (_, _, tag) in enumerate(keys) if tag[0] == "ours"}
    return order


def block_order(d: Design, sch: Scheduler, nz: Nuisance, rng: np.random.Generator,
                out: dict | None = None) -> list[int]:
    """Order (list of idx) in which our txs appear in the block, before landing losses."""
    n = len(d.txs)
    if sch.kind == "mix":
        if rng.random() < sch.p_rule:
            return block_order(d, sch.base_sched or SCHEDULERS[sch.base], nz, rng)
        return list(rng.permutation(n))
    if sch.kind == "random":
        return list(rng.permutation(n))
    arr = arrivals_ms(d, nz, rng)
    if sch.kind == "fifo":
        return sorted(range(n), key=lambda i: arr[i])
    if sch.kind == "fba":
        phase = rng.uniform(0, sch.fba_ms)
        buf = np.floor((arr + phase) / sch.fba_ms)
        tie = rng.random(n)  # "deterministic", rule unpublished
        return sorted(range(n), key=lambda i: (buf[i], -d.txs[i].priority_fee, tie[i]))
    return _threaded(d, arr, sch, nz, rng, out)


@dataclass
class SimWindow:
    design: Design
    landed: list[int]  # idx of landed txs, in block order
    arrival_us: list[float]  # per idx, for arrival-based rules
    span: int | None = None  # block positions spanned by landed txs incl. background (thread models)


def simulate_window(arm: str, sch: Scheduler, nz: Nuisance, rng: np.random.Generator,
                    cfg: ArmConfig = ArmConfig(), x_gap_ms: int | None = None) -> SimWindow:
    if x_gap_ms is None:
        x_gap_ms = int(rng.choice(cfg.x_gaps_ms))
    d = design(cfg, arm, rng, x_gap_ms=x_gap_ms)
    extra: dict = {}
    order = block_order(d, sch, nz, rng, extra)
    keep = rng.random(len(order)) < nz.p_land
    landed = [i for i in order if keep[i]]
    # Recorded send times: the burst loop sends in send_rank order (offset first), about 5 us
    # apart (dry-run records, 2026-10-23). Arrival at the leader is modelled separately, with
    # network jitter, in arrivals_ms.
    arrival_us = [t.send_offset_us + t.send_rank * 5.0 for t in d.txs]
    span = None
    if "pos" in extra and landed:
        ps = [extra["pos"][i] for i in landed]
        span = max(ps) - min(ps) + 1
    return SimWindow(design=d, landed=landed, arrival_us=arrival_us, span=span)


def with_nuisance(nz: Nuisance, **kw) -> Nuisance:
    return replace(nz, **kw)
