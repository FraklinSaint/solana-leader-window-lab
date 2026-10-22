//! Window planner: turns the leader schedule + targets + clock fit into a list of windows to
//! hit, each with an arm design, a send time and an expected cost. Pure function of its
//! inputs, so the same code path serves dry-run and live.

use crate::arms::{self, Arm, ArmConfig, WindowDesign};
use crate::clock::Fit;
use serde::{Deserialize, Serialize};
use slwlab_common::fees::total_fee;
use slwlab_common::guard;
use slwlab_common::limits::PLANNED_SPEND_BUDGET_LAMPORTS;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanConfig {
    pub campaign_seed: u64,
    /// Burst start relative to the predicted observation time of the window's first slot.
    pub offset_ms: i64,
    /// Minimum time between the bursts of two planned windows.
    pub min_spacing_ms: u64,
    /// Windows starting sooner than this are skipped (blockhash fetch + signing + connect).
    pub min_lead_ms: u64,
    pub min_live_epoch: u64,
    pub max_windows: usize,
    pub lamports_per_signature: u64,
    /// Remaining planning budget (project budget minus ledger spend so far).
    pub budget_lamports: u64,
    pub arms: Vec<Arm>,
    pub arm_cfg: ArmConfig,
}

impl Default for PlanConfig {
    fn default() -> Self {
        Self {
            campaign_seed: 0,
            offset_ms: -100,
            min_spacing_ms: 5_000,
            min_lead_ms: 3_000,
            min_live_epoch: guard::DEFAULT_MIN_LIVE_EPOCH,
            max_windows: usize::MAX,
            lamports_per_signature: 5_000,
            budget_lamports: PLANNED_SPEND_BUDGET_LAMPORTS,
            arms: Arm::ALL.to_vec(),
            arm_cfg: ArmConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub identity: String,
    pub vote_account: String,
    pub stratum: String,
    pub tpu_quic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedWindow {
    pub window_id: String,
    pub identity: String,
    pub vote_account: String,
    pub stratum: String,
    pub tpu_quic: Option<String>,
    pub first_slot: u64,
    pub n_slots: u64,
    pub epoch: u64,
    pub predicted_first_slot_unix_ns: i128,
    pub send_at_unix_ns: i128,
    /// Time between planning and the burst; must be >= min_lead.
    pub lead_ms: i64,
    pub design: WindowDesign,
    pub expected_cost_lamports: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skip {
    pub first_slot: u64,
    pub identity: String,
    pub reason: String,
}

/// Group a contiguous run of slot leaders (starting at `start_slot`) into leader windows.
/// Back-to-back windows of the same leader merge into one window of 8+ slots.
pub fn windows_from_leaders(start_slot: u64, leaders: &[String]) -> Vec<(u64, u64, String)> {
    let mut out: Vec<(u64, u64, String)> = vec![];
    for (i, l) in leaders.iter().enumerate() {
        let slot = start_slot + i as u64;
        match out.last_mut() {
            Some((first, n, who)) if who == l && *first + *n == slot => *n += 1,
            _ => out.push((slot, 1, l.clone())),
        }
    }
    out
}

pub fn plan_windows(
    start_slot: u64,
    leaders: &[String],
    targets: &[Target],
    fit: &Fit,
    now_unix_ns: i128,
    cfg: &PlanConfig,
    shared_sink: Option<solana_pubkey::Pubkey>,
) -> (Vec<PlannedWindow>, Vec<Skip>) {
    let by_id: HashMap<&str, &Target> = targets.iter().map(|t| (t.identity.as_str(), t)).collect();
    let mut planned = vec![];
    let mut skips = vec![];
    let mut per_validator: BTreeMap<String, usize> = BTreeMap::new();
    let mut last_send: Option<i128> = None;
    let mut spent = 0u64;
    for (first_slot, n_slots, leader) in windows_from_leaders(start_slot, leaders) {
        let Some(t) = by_id.get(leader.as_str()) else {
            continue;
        };
        let skip = |reason: String| Skip {
            first_slot,
            identity: leader.clone(),
            reason,
        };
        // A window cut at the edge of the fetched schedule may be incomplete: skip it.
        if first_slot == start_slot || first_slot + n_slots == start_slot + leaders.len() as u64 {
            skips.push(skip("window truncated by schedule range".into()));
            continue;
        }
        if let Err(e) = guard::check_live_slot(first_slot, cfg.min_live_epoch) {
            skips.push(skip(e.to_string()));
            continue;
        }
        if t.tpu_quic.is_none() {
            skips.push(skip("no tpuQuic address in gossip".into()));
            continue;
        }
        let predicted = fit.predict_ns(first_slot) as i128;
        let send_at = predicted + i128::from(cfg.offset_ms) * 1_000_000;
        let lead_ms = ((send_at - now_unix_ns) / 1_000_000) as i64;
        if lead_ms < cfg.min_lead_ms as i64 {
            skips.push(skip(format!("lead {lead_ms} ms < min {}", cfg.min_lead_ms)));
            continue;
        }
        if let Some(prev) = last_send
            && send_at - prev < i128::from(cfg.min_spacing_ms) * 1_000_000
        {
            skips.push(skip("too close to previous planned window".into()));
            continue;
        }
        if planned.len() >= cfg.max_windows {
            skips.push(skip("max_windows reached".into()));
            continue;
        }
        let k = per_validator.entry(leader.clone()).or_default();
        let vseed = arms::window_seed(cfg.campaign_seed, &leader);
        // Balanced per validator: blocks of all arms, shuffled per validator.
        let arm_seq = arms::assign_arms(vseed, *k + 1, &cfg.arms);
        let arm = arm_seq[*k];
        let n_x = arm_seq.iter().filter(|&&a| a == Arm::X).count();
        let x_gap = cfg.arm_cfg.x_gaps_ms[(n_x.max(1) - 1) % cfg.arm_cfg.x_gaps_ms.len()];
        let window_id = format!("{first_slot}-{}", &leader[..8.min(leader.len())]);
        let seed = arms::window_seed(cfg.campaign_seed, &window_id);
        let design = arms::design(&cfg.arm_cfg, arm, x_gap, seed, shared_sink);
        let cost: u64 = design
            .txs
            .iter()
            .map(|tx| {
                total_fee(
                    cfg.lamports_per_signature,
                    1,
                    tx.cu_price_micro_lamports,
                    tx.cu_limit,
                )
            })
            .sum();
        if spent + cost > cfg.budget_lamports {
            skips.push(skip(format!(
                "planning budget: {spent} + {cost} > {}",
                cfg.budget_lamports
            )));
            continue;
        }
        spent += cost;
        *k += 1;
        last_send = Some(send_at);
        planned.push(PlannedWindow {
            window_id,
            identity: leader.clone(),
            vote_account: t.vote_account.clone(),
            stratum: t.stratum.clone(),
            tpu_quic: t.tpu_quic.clone(),
            first_slot,
            n_slots,
            epoch: first_slot / guard::SLOTS_PER_EPOCH,
            predicted_first_slot_unix_ns: predicted,
            send_at_unix_ns: send_at,
            lead_ms,
            design,
            expected_cost_lamports: cost,
        });
    }
    (planned, skips)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fit() -> Fit {
        Fit {
            t0_ns: 1e18,
            slot0: 460_100_000,
            ns_per_slot: 212e6,
            n_used: 100,
            n_rejected: 0,
            resid_ms: 1.0,
        }
    }

    fn leaders(start: u64, pattern: &[&str], reps: usize) -> Vec<String> {
        assert_eq!(start % 4, 0);
        let mut v = vec![];
        for _ in 0..reps {
            for p in pattern {
                v.extend(std::iter::repeat_n(p.to_string(), 4));
            }
        }
        v
    }

    fn target(id: &str) -> Target {
        Target {
            identity: id.into(),
            vote_account: format!("vote-{id}"),
            stratum: "Agave".into(),
            tpu_quic: Some("1.1.1.1:8009".into()),
        }
    }

    #[test]
    fn groups_and_merges_windows() {
        let l: Vec<String> = ["a", "a", "a", "a", "a", "a", "a", "a", "b", "b", "b", "b"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let w = windows_from_leaders(100, &l);
        assert_eq!(w, vec![(100, 8, "a".into()), (108, 4, "b".into())]);
    }

    #[test]
    fn refuses_pre_alpenglow_and_pre_stabilization_slots() {
        let start = 459_652_000; // inside epoch 1064, straddling the boundary
        let l = leaders(start, &["tgt", "x", "y"], 600);
        let f = Fit {
            slot0: start,
            ..fit()
        };
        let (p, s) = plan_windows(
            start,
            &l,
            &[target("tgt")],
            &f,
            0,
            &PlanConfig::default(),
            None,
        );
        assert!(p.is_empty());
        assert!(
            s.iter()
                .any(|s| s.reason.contains("before ALPENGLOW_GENESIS_SLOT"))
        );
        assert!(s.iter().any(|s| s.reason.contains("stabilization")));
    }

    #[test]
    fn plans_with_spacing_lead_budget_and_balanced_arms() {
        let start = 460_100_000;
        let l = leaders(start, &["tgt", "x", "tgt2", "y", "z", "w"], 400);
        let now = fit().predict_ns(start) as i128;
        let cfg = PlanConfig {
            campaign_seed: 3,
            ..Default::default()
        };
        let (p, s) = plan_windows(
            start,
            &l,
            &[target("tgt"), target("tgt2")],
            &fit(),
            now,
            &cfg,
            None,
        );
        assert!(!p.is_empty());
        for w in p.windows(2) {
            assert!(w[1].send_at_unix_ns - w[0].send_at_unix_ns >= 5_000_000_000);
        }
        assert!(p.iter().all(|w| w.lead_ms >= 3_000));
        assert!(s.iter().any(|s| s.reason.contains("min")));
        let tgt: Vec<_> = p.iter().filter(|w| w.identity == "tgt").collect();
        for arm in Arm::ALL {
            let n = tgt.iter().filter(|w| w.design.arm == arm).count();
            assert!(n >= tgt.len() / 5 - 1, "{arm:?} {n}");
        }
        // budget
        let tight = PlanConfig {
            budget_lamports: 100_000,
            ..cfg
        };
        let (p2, s2) = plan_windows(start, &l, &[target("tgt")], &fit(), now, &tight, None);
        assert!(p2.iter().map(|w| w.expected_cost_lamports).sum::<u64>() <= 100_000);
        assert!(s2.iter().any(|s| s.reason.contains("planning budget")));
    }

    #[test]
    fn deterministic() {
        let start = 460_100_000;
        let l = leaders(start, &["tgt", "x", "y"], 200);
        let now = fit().predict_ns(start) as i128;
        let cfg = PlanConfig::default();
        let a = plan_windows(start, &l, &[target("tgt")], &fit(), now, &cfg, None).0;
        let b = plan_windows(start, &l, &[target("tgt")], &fit(), now, &cfg, None).0;
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }
}
