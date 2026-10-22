//! Scheduler priority functions, transcribed from source (docs/VERIFIED_FACTS.md F6).
//!
//! * Agave v4.3.2 (a2033000): `core/src/transaction_priority.rs:32-66`,
//!   `cost-model/src/cost_model.rs:56-77,181-203`, `cost-model/src/block_cost_limits.rs:10-20`,
//!   `runtime/src/bank/fee_distribution.rs:97-106`.
//! * Firedancer v26.10.4 (b14bff44): `src/disco/pack/fd_pack.c:1036-1062` (rewards),
//!   `src/disco/pack/fd_pack.c:227` (COMPARE_WORSE), `src/disco/pack/fd_pack_cost.h:337-565` (cost).
//!
//! Only the terms that can be non-zero for our transactions are modelled: no precompile
//! signatures, no account allocations, legacy messages (ComputeBudget instructions honoured).

use crate::fees::priority_fee_lamports;
use std::cmp::Ordering;

pub const SIGNATURE_COST: u64 = 720;
pub const WRITE_LOCK_UNITS: u64 = 300;
pub const INSTRUCTION_DATA_BYTES_PER_CU: u64 = 4;
pub const HEAP_COST_PER_PAGE: u64 = 8;
pub const ACCOUNT_DATA_PAGE_SIZE: u64 = 32 * 1024;
pub const MAX_LOADED_ACCOUNTS_DATA_SIZE_BYTES: u32 = 64 * 1024 * 1024;
pub const BURN_PERCENT: u64 = 50;
/// Firedancer hardcodes this instead of reading the bank (`fd_pack.h:21`).
pub const FD_PACK_FEE_PER_SIGNATURE: u64 = 5000;
pub const AGAVE_PRIORITY_MULTIPLIER: u64 = 1_000_000;

/// The cost-relevant shape of a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TxShape {
    pub n_signatures: u64,
    pub n_writable_accounts: u64,
    /// Sum of `data.len()` over all instructions (ComputeBudget ones included).
    pub instruction_data_len: u64,
    pub cu_limit: u32,
    pub cu_price_micro_lamports: u64,
    /// `None` = no SetLoadedAccountsDataSizeLimit instruction, i.e. the 64 MiB default.
    pub loaded_accounts_data_size_limit: Option<u32>,
}

impl TxShape {
    pub fn priority_fee(&self) -> u64 {
        priority_fee_lamports(self.cu_price_micro_lamports, self.cu_limit)
    }

    fn loaded_bytes(&self) -> u64 {
        u64::from(
            self.loaded_accounts_data_size_limit
                .unwrap_or(MAX_LOADED_ACCOUNTS_DATA_SIZE_BYTES)
                .min(MAX_LOADED_ACCOUNTS_DATA_SIZE_BYTES),
        )
    }
}

/// Block-limit cost used by both clients (identical terms for our transaction class).
pub fn cost_units(s: &TxShape) -> u64 {
    let pages = s.loaded_bytes().div_ceil(ACCOUNT_DATA_PAGE_SIZE);
    SIGNATURE_COST * s.n_signatures
        + WRITE_LOCK_UNITS * s.n_writable_accounts
        + s.instruction_data_len / INSTRUCTION_DATA_BYTES_PER_CU
        + u64::from(s.cu_limit)
        + HEAP_COST_PER_PAGE * pages
}

/// Leader's share: priority fee + unburned half of the signature fee.
pub fn agave_reward(s: &TxShape, lamports_per_signature: u64) -> u64 {
    let sig_fee = lamports_per_signature * s.n_signatures;
    let burn = sig_fee * BURN_PERCENT / 100;
    s.priority_fee().saturating_add(sig_fee - burn)
}

/// Agave scheduler priority: `reward * 1_000_000 / (cost + 1)`, integer division.
pub fn agave_priority(s: &TxShape, lamports_per_signature: u64) -> u64 {
    agave_reward(s, lamports_per_signature)
        .saturating_mul(AGAVE_PRIORITY_MULTIPLIER)
        .saturating_div(cost_units(s).saturating_add(1))
}

/// Firedancer pack rewards (u32-saturated in pack; our values are far below that).
pub fn fd_rewards(s: &TxShape) -> u64 {
    FD_PACK_FEE_PER_SIGNATURE * s.n_signatures * BURN_PERCENT / 100 + s.priority_fee()
}

/// Firedancer ordering: `Greater` means `a` is scheduled before `b`.
/// Exact rational comparison of rewards/compute_est (COMPARE_WORSE, fd_pack.c:227).
pub fn fd_compare(a: &TxShape, b: &TxShape) -> Ordering {
    let lhs = u128::from(fd_rewards(a)) * u128::from(cost_units(b));
    let rhs = u128::from(fd_rewards(b)) * u128::from(cost_units(a));
    lhs.cmp(&rhs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(cu_limit: u32, price: u64) -> TxShape {
        // SetComputeUnitLimit(5) + SetComputeUnitPrice(9) + SetLoadedAccountsDataSizeLimit(5)
        // + system transfer(12) = 31 bytes of instruction data.
        TxShape {
            n_signatures: 1,
            n_writable_accounts: 1,
            instruction_data_len: 31,
            cu_limit,
            cu_price_micro_lamports: price,
            loaded_accounts_data_size_limit: Some(32 * 1024),
        }
    }

    #[test]
    fn cost_hand_computed() {
        // 720 + 300 + 31/4=7 + 1000 + 8*1 = 2035
        assert_eq!(cost_units(&shape(1000, 0)), 2035);
        // default 64 MiB limit: 2048 pages * 8 = 16384 instead of 8
        let mut s = shape(1000, 0);
        s.loaded_accounts_data_size_limit = None;
        assert_eq!(cost_units(&s), 2035 - 8 + 16_384);
        // one byte over a page boundary costs a second page
        s.loaded_accounts_data_size_limit = Some(32 * 1024 + 1);
        assert_eq!(cost_units(&s), 2035 + 8);
    }

    #[test]
    fn agave_priority_hand_computed() {
        // reward = ceil(1_000_000*1000/1e6)=1000 + 5000-2500 = 3500
        // priority = 3500 * 1e6 / 2036 = 1_719_056 (floor)
        let s = shape(1000, 1_000_000);
        assert_eq!(agave_reward(&s, 5000), 3500);
        assert_eq!(agave_priority(&s, 5000), 1_719_056);
    }

    #[test]
    fn fd_rewards_hand_computed() {
        assert_eq!(fd_rewards(&shape(1000, 1_000_000)), 3500);
        assert_eq!(fd_rewards(&shape(1000, 0)), 2500);
    }

    #[test]
    fn ratio_rules_reverse_total_fee_rule_in_arm_r_regime() {
        // Arm R: equal price, different CU limits. Below the crossover price
        // (~2500e6/C0 micro-lamports), larger limits LOWER reward/cost.
        let small = shape(1000, 200_000);
        let large = shape(8000, 200_000);
        assert!(large.priority_fee() > small.priority_fee()); // R_TOTAL: large first
        assert!(agave_priority(&small, 5000) > agave_priority(&large, 5000)); // R_RATIO: small first
        assert_eq!(fd_compare(&small, &large), Ordering::Greater);
    }

    #[test]
    fn fd_and_agave_agree_on_strict_orderings() {
        for (a, b) in [((1000, 10), (1000, 20)), ((1000, 200_000), (8000, 200_000))] {
            let (sa, sb) = (shape(a.0, a.1), shape(b.0, b.1));
            assert_eq!(
                fd_compare(&sa, &sb),
                agave_priority(&sa, 5000).cmp(&agave_priority(&sb, 5000))
            );
        }
    }
}
