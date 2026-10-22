//! Experimental arms. Each window gets one arm; the arm fixes the N transactions' fee
//! parameters, their send order and inter-send gaps. Assignment and within-arm randomization
//! come from a ChaCha RNG seeded by `(campaign_seed, window_id)`, recorded in the plan.
//!
//! | Arm | price | CU limit | send order | discriminates |
//! |---|---|---|---|---|
//! | P | geometric ladder | equal | random | fee rules vs FIFO/random |
//! | F | equal | equal | fixed, gap `f_gap` | FIFO and tie-break hypotheses |
//! | X | ascending in send order | equal | fixed, gap swept | fee vs arrival; batch (FBA) granularity |
//! | R | equal (below crossover) | ladder | random | R_PRICE vs R_TOTAL vs R_RATIO |
//! | C | geometric ladder | equal | random | conflict chain on one shared account |

use rand::{SeedableRng, seq::SliceRandom};
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use slwlab_common::cost::{TxShape, agave_priority, cost_units};
use solana_pubkey::Pubkey;
use std::cmp::Reverse;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Arm {
    P,
    F,
    X,
    R,
    C,
}

impl Arm {
    pub const ALL: [Arm; 5] = [Arm::P, Arm::F, Arm::X, Arm::R, Arm::C];
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmConfig {
    pub n: usize,
    pub cu_limit: u32,
    pub price_min: u64,
    pub price_max: u64,
    pub f_price: u64,
    pub f_gap_us: u64,
    pub x_gaps_ms: Vec<u64>,
    pub r_price: u64,
    pub r_cu_limits: Vec<u32>,
    pub p_gap_us: u64,
}

impl Default for ArmConfig {
    fn default() -> Self {
        Self {
            n: 8,
            cu_limit: 1_000,
            price_min: 10_000,
            price_max: 1_000_000,
            f_price: 50_000,
            f_gap_us: 10_000,
            x_gaps_ms: vec![0, 10, 25, 60, 120],
            // Crossover where reward/cost stops falling with the CU limit is
            // ~2500e6 / C0 µL/CU (C0 ≈ 1035 non-execution CU) ≈ 2.4M. 200k is far below it,
            // so R_RATIO = ascending CU limit and R_TOTAL = descending CU limit.
            r_price: 200_000,
            r_cu_limits: vec![1_000, 2_000, 3_000, 4_000, 5_000, 6_000, 7_000, 8_000],
            p_gap_us: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedTx {
    /// Index of the tx within the window; stable identifier in manifests.
    pub idx: usize,
    pub cu_limit: u32,
    pub cu_price_micro_lamports: u64,
    /// 0-based position in the send sequence.
    pub send_rank: usize,
    /// Offset of the planned send start from the burst start.
    pub send_offset_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowDesign {
    pub arm: Arm,
    pub gap_us: u64,
    pub seed: u64,
    pub txs: Vec<PlannedTx>,
    /// Arm C only: the shared writable account.
    pub shared_sink: Option<Pubkey>,
}

pub fn window_seed(campaign_seed: u64, window_id: &str) -> u64 {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(format!("{campaign_seed}:{window_id}").as_bytes());
    u64::from_le_bytes(d[..8].try_into().expect("8 bytes"))
}

/// Balanced arm assignment: arms are drawn in shuffled blocks of `Arm::ALL`, so every 5
/// consecutive windows contain each arm once (X gaps rotate across X windows).
pub fn assign_arms(campaign_seed: u64, n_windows: usize, enabled: &[Arm]) -> Vec<Arm> {
    let mut rng = ChaCha20Rng::seed_from_u64(campaign_seed);
    let mut out = Vec::with_capacity(n_windows);
    while out.len() < n_windows {
        let mut block = enabled.to_vec();
        block.shuffle(&mut rng);
        out.extend(block);
    }
    out.truncate(n_windows);
    out
}

fn geometric(min: u64, max: u64, n: usize) -> Vec<u64> {
    if n == 1 {
        return vec![max];
    }
    let r = (max as f64 / min as f64).powf(1.0 / (n as f64 - 1.0));
    let v: Vec<u64> = (0..n)
        .map(|i| (min as f64 * r.powi(i as i32)).round() as u64)
        .collect();
    debug_assert!(
        v.windows(2).all(|w| w[0] < w[1]),
        "ladder must be strictly increasing"
    );
    v
}

pub fn design(
    cfg: &ArmConfig,
    arm: Arm,
    x_gap_ms: u64,
    seed: u64,
    shared_sink: Option<Pubkey>,
) -> WindowDesign {
    let n = cfg.n;
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let mut order: Vec<usize> = (0..n).collect(); // order[rank] = idx
    let (prices, limits, gap_us): (Vec<u64>, Vec<u32>, u64) = match arm {
        Arm::P | Arm::C => {
            order.shuffle(&mut rng);
            (
                geometric(cfg.price_min, cfg.price_max, n),
                vec![cfg.cu_limit; n],
                cfg.p_gap_us,
            )
        }
        Arm::F => (vec![cfg.f_price; n], vec![cfg.cu_limit; n], cfg.f_gap_us),
        // idx order == send order and prices ascend with idx: the last-sent tx pays most.
        Arm::X => (
            geometric(cfg.price_min, cfg.price_max, n),
            vec![cfg.cu_limit; n],
            x_gap_ms * 1000,
        ),
        Arm::R => {
            order.shuffle(&mut rng);
            let mut l = cfg.r_cu_limits.clone();
            l.truncate(n);
            assert_eq!(l.len(), n, "r_cu_limits must have >= n entries");
            (vec![cfg.r_price; n], l, cfg.p_gap_us)
        }
    };
    let mut txs: Vec<PlannedTx> = (0..n)
        .map(|idx| PlannedTx {
            idx,
            cu_limit: limits[idx],
            cu_price_micro_lamports: prices[idx],
            send_rank: 0,
            send_offset_us: 0,
        })
        .collect();
    for (rank, &idx) in order.iter().enumerate() {
        txs[idx].send_rank = rank;
        txs[idx].send_offset_us = rank as u64 * gap_us;
    }
    WindowDesign {
        arm,
        gap_us,
        seed,
        txs,
        shared_sink: if arm == Arm::C { shared_sink } else { None },
    }
}

/// Shape used for priority predictions (matches txbuild's lock-free/conflict layouts).
pub fn shape_of(tx: &PlannedTx, conflict: bool) -> TxShape {
    TxShape {
        n_signatures: 1,
        n_writable_accounts: if conflict { 2 } else { 1 },
        instruction_data_len: 5 + 9 + 5 + 12,
        cu_limit: tx.cu_limit,
        cu_price_micro_lamports: tx.cu_price_micro_lamports,
        loaded_accounts_data_size_limit: Some(crate::txbuild::LOADED_ACCOUNTS_DATA_LIMIT),
    }
}

/// Predicted landing order (list of idx) under simple source-derived rules. Ties are left in
/// send order; Phase 1's analysis/rules.py is the authoritative implementation.
pub fn predict(d: &WindowDesign, rule: &str, lamports_per_signature: u64) -> Vec<usize> {
    let conflict = d.arm == Arm::C;
    let mut v: Vec<&PlannedTx> = d.txs.iter().collect();
    v.sort_by_key(|t| t.send_rank);
    match rule {
        "R_FIFO" => {}
        "R_PRICE" => v.sort_by_key(|t| Reverse(t.cu_price_micro_lamports)),
        "R_TOTAL" => v.sort_by_key(|t| Reverse(shape_of(t, conflict).priority_fee())),
        "R_RATIO_AGAVE" => v.sort_by_key(|t| {
            Reverse(agave_priority(
                &shape_of(t, conflict),
                lamports_per_signature,
            ))
        }),
        "COST" => v.sort_by_key(|t| cost_units(&shape_of(t, conflict))),
        other => panic!("unknown rule {other}"),
    }
    v.into_iter().map(|t| t.idx).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(arm: Arm) -> WindowDesign {
        design(
            &ArmConfig::default(),
            arm,
            25,
            42,
            Some(Pubkey::new_unique()),
        )
    }

    #[test]
    fn deterministic_given_seed() {
        let a = design(&ArmConfig::default(), Arm::P, 0, 7, None);
        let b = design(&ArmConfig::default(), Arm::P, 0, 7, None);
        assert_eq!(
            a.txs.iter().map(|t| t.send_rank).collect::<Vec<_>>(),
            b.txs.iter().map(|t| t.send_rank).collect::<Vec<_>>()
        );
        assert_eq!(window_seed(1, "w"), window_seed(1, "w"));
        assert_ne!(window_seed(1, "w"), window_seed(2, "w"));
    }

    #[test]
    fn arm_p_fee_rules_agree() {
        let w = d(Arm::P);
        let price = predict(&w, "R_PRICE", 5000);
        assert_eq!(price, predict(&w, "R_TOTAL", 5000));
        assert_eq!(price, predict(&w, "R_RATIO_AGAVE", 5000));
        assert_eq!(price, (0..8).rev().collect::<Vec<_>>());
    }

    #[test]
    fn arm_r_total_and_ratio_are_reversed() {
        let w = d(Arm::R);
        let total = predict(&w, "R_TOTAL", 5000);
        let mut ratio = predict(&w, "R_RATIO_AGAVE", 5000);
        assert_eq!(total, (0..8).rev().collect::<Vec<_>>()); // largest CU limit first
        ratio.reverse();
        assert_eq!(total, ratio);
        // and R_PRICE is all ties -> falls back to send order
        assert_eq!(predict(&w, "R_PRICE", 5000), predict(&w, "R_FIFO", 5000));
    }

    #[test]
    fn arm_x_fee_and_fifo_are_reversed() {
        let w = d(Arm::X);
        let mut fifo = predict(&w, "R_FIFO", 5000);
        fifo.reverse();
        assert_eq!(predict(&w, "R_PRICE", 5000), fifo);
        assert_eq!(w.gap_us, 25_000);
        assert_eq!(w.txs[3].send_offset_us, 75_000);
    }

    #[test]
    fn arm_f_all_equal_fixed_order() {
        let w = d(Arm::F);
        assert!(
            w.txs
                .iter()
                .all(|t| t.cu_price_micro_lamports == w.txs[0].cu_price_micro_lamports)
        );
        assert!(w.txs.iter().all(|t| t.send_rank == t.idx));
        assert!(w.shared_sink.is_none());
    }

    #[test]
    fn only_arm_c_gets_a_shared_sink() {
        for arm in Arm::ALL {
            assert_eq!(d(arm).shared_sink.is_some(), arm == Arm::C, "{arm:?}");
        }
    }

    #[test]
    fn balanced_assignment() {
        let a = assign_arms(9, 50, &Arm::ALL);
        for arm in Arm::ALL {
            assert_eq!(a.iter().filter(|&&x| x == arm).count(), 10);
        }
    }
}
