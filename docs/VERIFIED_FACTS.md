# Verified facts

Every constant and assumption in the code or report must trace back to an entry here.
On-chain facts can be re-derived with `python scripts/verify_facts.py`, which writes
`data/snapshots/facts_<UTC>.json` and the raw gzipped cluster, vote and Kobe snapshots.
Facts were first verified **2026-10-22 ~15:00–16:00 UTC** against `api.mainnet-beta.solana.com`,
which reported `solana-core 4.3.2` and feature-set `3383571666`.

Status legend: **VERIFIED** (checked against source or chain), **PARTIAL** (checked, with open
sub-points), **OPEN** (not yet resolved; the phase it is scheduled for is named).

---

## F1. Alpenglow boundary — VERIFIED (with a correction to the brief)

| Evidence | Value |
|---|---|
| Epoch 1064 first slot | 459,648,000 (= 1064 × 432,000) |
| Last block **with** vote txs | slot **459,652,999**: 1,051 txs, 649 vote txs; blockTime 1792590840 = 2026-10-21 13:54:00 UTC |
| Slots 459,653,000–459,653,007 | skipped (no blocks, from `getBlocksWithLimit`) |
| First block **without** vote txs | slot **459,653,008** (parent 459,652,999): 1,701 txs, 0 vote txs |
| Later samples | 459,653,100 / 459,660,000 / 460,000,000: 0 vote txs each |

**Correction:** Alpenglow did **not** activate at the start of epoch 1064. It activated 5,000 slots
into it, at slot `ALPENGLOW_GENESIS_SLOT = 459_653_000`, so the brief's constant is correct but
"activated at epoch 1064" is only approximately true. Epoch 1064 contains ~5,000 TowerBFT slots.
That is one more reason for the default stabilization margin `DEFAULT_MIN_LIVE_EPOCH = 1065`,
whose first slot is 460,080,000.

**Also new:** mainnet blocks contain **transaction version 1** (e.g. 799 of 1,701 txs in slot
459,653,008). `getBlock` with `maxSupportedTransactionVersion: 0` **fails** on these blocks
("Transaction version (1) is not supported"). The parser and `verify` must request
`maxSupportedTransactionVersion: 1`. The brief said 0; that would have broken ground-truth
collection.

## F2. Slot time — VERIFIED

- About **212 ms/slot** over long spans (459,700,000 → 460,000,000 and others).
  `getRecentPerformanceSamples` gives about 285 slots per 60 s, i.e. 210 ms.
- **Surprise:** slot time was already about 215–217 ms **before** Alpenglow
  (455,000,000 → 458,000,000: 217 ms; 459,600,000 → 459,640,000: 215 ms). So the shorter slot
  is not a marker of the Alpenglow boundary. We do not rely on it; the boundary is defined by slot (F1).
- `numNonVoteTransactions == numTransactions` in performance samples after the boundary,
  consistent with votes being off-chain.
- The clock model uses an online regression, not a constant (Phase 2).

## F3. Leader window — VERIFIED

- `getSlotLeaders` from 460,080,000, over 5,000 slots: runs of **4** consecutive slots (1,232 runs),
  plus 9 runs of 8 (the same leader drawn twice in a row). Every run starts at `slot % 4 == 0`.
- Agave: `last_of_consecutive_leader_slots = first + NUM_CONSECUTIVE_LEADER_SLOTS − 1`
  (`runtime/src/leader_schedule_utils.rs` @ v4.3.2).
- Window duration is 4 × ~212 ms ≈ 850 ms. **Unit of replication = one 4-slot window.** Two
  back-to-back windows of the same leader are treated as one 8-slot window, but are flagged.
- Epoch length is 432,000 slots, `warmup=false` (`getEpochSchedule`). At 212 ms that is ≈ 25.4 h per epoch.

## F4. Blockhash validity and rent — VERIFIED

- `getLatestBlockhash.lastValidBlockHeight − getBlockHeight` = **148–149** blocks at `processed`.
  That is ≈ 150 blocks ≈ 32 s at 212 ms if no slots are skipped. The sender fetches a fresh
  blockhash right before signing each burst and tracks `lastValidBlockHeight` per tx.
- `getMinimumBalanceForRentExemption(0)` = **650,240** lamports. The brief's mental model of
  890,880 is outdated. **Never hardcode**: `fund` queries it at run time.

## F5. Fee formula — VERIFIED

Source: `compute-budget/src/compute_budget_limits.rs` and `fee/src/lib.rs` @ agave v4.3.2
(commit `a203300045b405f86c9c84308b2eae76af69ac3b`).

```
priority_fee = ceil(cu_price_micro_lamports × cu_limit / 1_000_000)   (u128, saturating)
signature_fee = (n_tx_sigs + n_precompile_sigs) × lamports_per_signature
total_fee = signature_fee + priority_fee
```

`getFeeForMessage` on mainnet, for one signature and a CU limit of 1,000, at prices
0, 1, 999, 1000, 1001 and 1e6 µL/CU, returned 5000, 5001, 5001, 5001, 5002 and 6000.
This matches the formula exactly, with `lamports_per_signature = 5000`.
Unit-tested in `crates/common/src/fees.rs`.

## F6. Scheduler priority functions — VERIFIED from source

### Agave v4.3.2 (`a2033000`), the reference for vanilla Agave, JitoLabs and probably HarmonicAgave/Rakurai fallbacks

**Correction:** there is **no prio-graph scheduler** in v4.3.2. `core/src/banking_stage/transaction_scheduler/`
contains only `greedy_scheduler.rs`; `prio_graph_scheduler.rs` no longer exists.

- Priority, from `core/src/transaction_priority.rs::calculate_priority_and_cost`:
  ```
  reward   = priority_fee + (signature_fee − signature_fee × 50 / 100)    # burn 50% of base fee
  cost     = CostModel::calculate_cost_for_executed_transaction(tx, cu_limit, loaded_accounts_data_size_limit)
  priority = reward × 1_000_000 / (cost + 1)                               # integer division
  ```
  Burn is `DEFAULT_BURN_PERCENT` = 50, from `runtime/src/bank/fee_distribution.rs`.
- Cost, from `cost-model/src/cost_model.rs` and `block_cost_limits.rs`:
  ```
  cost = 720 × n_sigs                        # SIGNATURE_COST = 30 × 24
       + 300 × n_writable_accounts           # WRITE_LOCK_UNITS
       + instruction_data_len / 4            # INSTRUCTION_DATA_BYTES_COST = 140/30 = 4 (int)
       + cu_limit                            # the *requested* limit, not usage
       + 8 × ceil(loaded_accounts_bytes_limit / 32768)   # DEFAULT_HEAP_COST, 32 KiB pages
       + allocated_accounts_data_size        # 0 for our txs
  ```
  **Design consequence:** without `SetLoadedAccountsDataSizeLimit`, the limit defaults to
  `MAX_LOADED_ACCOUNTS_DATA_SIZE_BYTES` (64 MiB). That adds 2048 × 8 = 16,384 CU of cost, which
  dominates the denominator for small txs. We **always** set an explicit small loaded-data limit,
  so that the R_RATIO prediction is driven by `cu_limit`.
- Queue: `BTreeSet<TransactionPriorityId{priority, id}>`, popped with `pop_last()`
  (`transaction_state_container.rs`). **Ties on priority are broken by the larger `id`**.
  `id` is a `Slab` key, and Slab reuses the lowest free slot. So the tie-break among equal
  priorities is "highest slab index first". That is **not arrival order** (neither FIFO nor LIFO
  in general). Prediction for Arm F on Agave: ties are effectively unordered with respect to our
  send order. This is a sharp, falsifiable prediction.
- Dispatch: the greedy scheduler pops in priority order and assigns each non-conflicting tx to the
  **least-loaded worker thread** (`scheduler_common.rs::select_thread`). Batches are at most
  `TARGET_NUM_TRANSACTIONS_PER_BATCH = 64` txs or the entry-byte target. Batches on different
  threads execute in parallel and are recorded in completion order. **Consequence:** even a
  perfect priority scheduler can interleave our 8 non-conflicting txs across threads, so block
  position is a *noisy* image of pop order. The simulator must model this, and the positive
  control is not expected to be STRICT a priori. We pre-register this.
- Timing: before its slots, a leader **holds** received txs. `decision_maker.rs`: `Hold` when
  `slots_until_leader < FORWARD_TRANSACTIONS_TO_LEADER_AT_SLOT_OFFSET`, otherwise
  `ForwardAndHold`/`Forward`. Under Alpenglow, slot progress is time-based (`alpenglow_timing.rs`).
  **Design consequence:** a burst that arrives shortly *before* the window sits in the buffer
  and is popped by priority at window start. That gives contention among our own txs without
  needing external load. This is the hypothesis behind the offset sweep (−300/−100/0/+100 ms).
- Saturation: there is a `SchedulerPriorityFloor` that, under buffer saturation, drops
  low-priority packets (`scheduler_controller.rs::update_scheduler_priority_floor`). Our
  lowest-price txs may therefore be **dropped, not reordered**, when the leader is saturated.
  Landing rate per price is a secondary outcome.

### Firedancer v26.10.4 (`b14bff44`), the reference for Firedancer, HarmonicFiredancer and FireBAM fallbacks

- `src/disco/pack/fd_pack.c::fd_pack_estimate_rewards_and_compute`:
  ```
  rewards     = 5000 × (n_sigs + n_precompile_sigs) × 50 / 100 + priority_fee   # FD_PACK_FEE_PER_SIGNATURE hardcoded
  compute_est = fd_pack_compute_cost(...)   # 720/sig, 300/writable, instr_data/4, cu_limit, 8/32KiB loaded data
  ```
  Ordering: treap with `COMPARE_WORSE(x,y) = x.rewards × y.compute_est < y.rewards × x.compute_est`.
  That is an **exact rational comparison** of rewards/cost, not integer-divided.
- So R_RATIO_FD and R_RATIO_AGAVE are the same rule up to integer rounding and the `+1`. They
  differ only on near-ties, and the arms are designed so that they agree, and say so.
- V1 transactions take their fee and limits from a config mask and *ignore* ComputeBudget
  instructions (`fd_pack_cost.h`). We send **legacy** transactions only, so both clients parse our
  ComputeBudget instructions identically.
- OPEN (Phase 1): the tie order of equal `rewards/compute_est` in the treap, and the microblock
  and bank-tile dispatch (`fd_pack_pacing.h`, multiple bank tiles). Expect a thread-interleaving
  noise source similar to Agave's.

## F7. Client identification — VERIFIED (new primary source)

- `getClusterNodes` now returns a **`clientId`** field for every node (gossip contact info).
  Stake-weighted on 2026-10-22:

  | clientId | version | stake % | voting nodes |
  |---|---|---|---|
  | AgaveBam | 4.3.2 | 36.4 | 362 |
  | HarmonicAgave | 4.3.2 | 23.8 | 63 |
  | JitoLabs | 4.3.2 | 21.8 | 139 |
  | Firedancer | 26.10.4 | 6.0 | 13 |
  | HarmonicFiredancer | 26.10.4 | 6.0 | 21 |
  | JitoLabs | 4.3.0 | 1.5 | 6 |
  | FireBAM | 26.10.4 | 1.4 | 9 |
  | Rakurai | 4.3.2 | 1.2 | 10 |
  | Agave | 4.3.2 | 1.0 | 13 |

  No "Frankendancer" `clientId` appears, consistent with the brief.
- `clientId` is **self-reported**, so the confidence rules are:
  - **high**: `clientId` agrees with an independent source (Kobe `running_jito`/`running_bam`,
    validators.app software client).
  - **medium**: `clientId` only.
  - **low**: version string only.
- Kobe `running_jito=true` does **not** contradict `clientId=Firedancer`. Firedancer ships its own
  Jito block-engine client: `src/disco/bundle/fd_bundle_tile.c:172` (publishes block engine `"jito"`),
  `src/disco/bundle/fd_bundle_client.c:188,210` (`BlockEngineValidator` RPCs), and pack schedules
  bundles next to ordinary transactions (`src/disco/pack/fd_pack.h:190-198,474-487`;
  `src/disco/pack/fd_pack_tile.c:681` tags them `FD_TXN_M_TPU_SOURCE_BUNDLE`), all at `b14bff44`.
  Ordinary (non-bundle) transactions are still ordered by fd_pack. On 2026-10-22, 17 of 19
  Firedancer-labelled voting validators had `running_jito=true`. The confidence rule therefore
  checks only `running_bam` for Firedancer. A Kobe entry whose deciding field is `null` (e.g.
  `running_bam: null` for 248 AgaveBam nodes) is *unknown*, so confidence is **medium**, not low.
- Jito Kobe API `https://kobe.mainnet.jito.network/api/v1/validators` works without a key. Per
  validator it returns `running_jito`, `running_bam`, `bam_connection_rate`, `active_stake`,
  `identity_account` and `vote_account`. `bam_connection_rate < 1` means BAM is only intermittent:
  record it per window.
- validators.app: optional, via `VALIDATORS_APP_API_KEY`. Not required.
- **Harmonic strategy is not externally observable.** Per docs.harmonic.gg/concepts/scheduling-strategies,
  operators choose `fba` (the default), `fifo` or `mrev` via `--strategy`. So "HarmonicAgave" is
  three possible rules. Inferring which one a given validator uses is itself a result of this
  study, which is why conclusions are per validator.
  - FBA: 50 ms buffers; within a buffer, sort by descending priority fee + tips; ties broken
    "deterministically" (rule unpublished); arrival order within a buffer is ignored.
  - FIFO: strict arrival timestamp **at the builder**; fees are ignored.
  - MREV: proprietary, not SFDP-compliant.
- BAM: scheduling happens in TEE BAM nodes; the validator runs a FIFO executor over pre-sequenced
  batches. The validator advertises the BAM node via its TPU ports. *Implication:* for AgaveBam
  and FireBAM, sending to `tpuQuic` may reach the **BAM node**, not the validator. The path is
  part of the treatment and is reported. Sources: helius.dev/blog/block-assembly-marketplace-bam,
  chorus.one research.
- Rakurai: a jito-solana fork that loads a proprietary scheduler binary, and falls back to Jito-Agave
  behaviour if it is disabled (Figment write-up). Closed source, so no source-derived prediction.

## F8. Producer identification — VERIFIED

- `getBlock(..., rewards: true)` still contains exactly one `rewardType: "Fee"` entry on both sides of
  the boundary. Its `pubkey` is the producer identity. We cross-check it with `getSlotLeaders` for the
  slot, and use `parentSlot` chains for skip detection.

## F9. TPU / QUIC — PARTIAL

- ALPN `b"solana-tpu"` (`streamer/src/nonblocking/quic.rs`, `ALPN_TPU_PROTOCOL_ID`).
- Unstaked limits in Agave v4.3.2 `streamer`:
  - `QUIC_MAX_UNSTAKED_CONCURRENT_STREAMS = 128`
  - `DEFAULT_MAX_QUIC_CONNECTIONS_PER_UNSTAKED_PEER = 8`
  - `DEFAULT_MAX_UNSTAKED_CONNECTIONS = 2000`
  - throttle `MAX_UNSTAKED_TPS = 200`, i.e. 20 streams per 100 ms `STREAM_THROTTLING_INTERVAL`

  A burst of 8 fits within one interval. OPEN (Phase 2): confirm whether the 20/100 ms budget is
  per connection or shared, by reading `stream_throttle.rs` fully.
- Crates: `solana-tpu-client-next` and `solana-streamer` **4.3.2** are on crates.io and match the
  mainnet Agave version. Phase 2 decision: use `solana-tpu-client-next` if it lets us pre-open
  connections and control per-stream timing. Otherwise write a minimal quinn client (ALPN
  solana-tpu, self-signed cert, one uni stream per tx).
- Toolchain: Agave v4.3.2 requires Rust **1.97.1** (`rust-toolchain.toml`). The workspace and
  Docker builder are pinned to it.

## F10. Not verified / open list

| Item | Phase |
|---|---|
| Firedancer treap tie-break and bank-tile interleave | 1 |
| Per-connection vs global unstaked stream throttle | 2 |
| Whether BAM and Harmonic validators accept plain TPU txs at all, and from whom (landing rate) | 5 (calibration) |
| CU actually consumed by our minimal tx (simulateTransaction) | 2 |
| Whether `--strategy` for a Harmonic validator can be read from any public endpoint | 4 |

## Source pins

| Source | Ref | Commit |
|---|---|---|
| anza-xyz/agave | v4.3.2 | a203300045b405f86c9c84308b2eae76af69ac3b |
| firedancer-io/firedancer | v26.10.4 | b14bff444e37977ad5eddcf811bf74e027203ac1 |
| docs.harmonic.gg | concepts/scheduling-strategies | fetched 2026-10-22 |
| Jito Kobe API | /api/v1/validators | fetched 2026-10-22 |
