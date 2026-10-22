//! Fee math, transcribed from the verified sources (docs/VERIFIED_FACTS.md, F5).
//!
//! `lamports_per_signature` is never hardcoded at call sites: the sender fetches it with
//! getFeeForMessage and cross-checks it against [`total_fee`] before every run.

/// Micro-lamports per lamport (agave compute-budget/src/compute_budget_limits.rs).
pub const MICRO_LAMPORTS_PER_LAMPORT: u128 = 1_000_000;

/// Priority fee in lamports: ceil(cu_price_micro_lamports * cu_limit / 1e6), saturating.
pub fn priority_fee_lamports(cu_price_micro_lamports: u64, cu_limit: u32) -> u64 {
    let micro = u128::from(cu_price_micro_lamports).saturating_mul(u128::from(cu_limit));
    let lamports = micro
        .saturating_add(MICRO_LAMPORTS_PER_LAMPORT - 1)
        .checked_div(MICRO_LAMPORTS_PER_LAMPORT)
        .unwrap_or(u128::MAX);
    u64::try_from(lamports).unwrap_or(u64::MAX)
}

/// Total fee charged for a transaction with `n_signatures` (no precompile signatures).
/// This is also the worst case reserved in the spend ledger: the fee is charged in full
/// whether the transaction succeeds or fails, and nothing is charged if it never lands.
pub fn total_fee(
    lamports_per_signature: u64,
    n_signatures: u64,
    cu_price_micro_lamports: u64,
    cu_limit: u32,
) -> u64 {
    lamports_per_signature
        .saturating_mul(n_signatures)
        .saturating_add(priority_fee_lamports(cu_price_micro_lamports, cu_limit))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values were observed with getFeeForMessage on mainnet, 2026-10-22
    // (lamports_per_signature = 5000, CU limit 1000): see VERIFIED_FACTS F5.
    #[test]
    fn matches_mainnet_get_fee_for_message() {
        assert_eq!(total_fee(5000, 1, 0, 1000), 5000);
        assert_eq!(total_fee(5000, 1, 1, 1000), 5001);
        assert_eq!(total_fee(5000, 1, 999, 1000), 5001);
        assert_eq!(total_fee(5000, 1, 1000, 1000), 5001);
        assert_eq!(total_fee(5000, 1, 1001, 1000), 5002);
        assert_eq!(total_fee(5000, 1, 1_000_000, 1000), 6000);
    }

    #[test]
    fn priority_fee_rounds_up() {
        assert_eq!(priority_fee_lamports(0, 1_400_000), 0);
        assert_eq!(priority_fee_lamports(1, 1), 1);
        assert_eq!(priority_fee_lamports(1_000_000, 1), 1);
        assert_eq!(priority_fee_lamports(1_000_001, 1), 2);
    }

    #[test]
    fn priority_fee_saturates() {
        assert_eq!(priority_fee_lamports(u64::MAX, u32::MAX), u64::MAX);
        assert_eq!(total_fee(u64::MAX, 2, 0, 0), u64::MAX);
    }
}
