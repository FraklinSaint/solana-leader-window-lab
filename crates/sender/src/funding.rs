//! Funding, sweep, balance snapshots and end-of-run reconciliation.
//!
//! Money only moves between our own wallets (master, payers, Arm C sink), so between two
//! balance snapshots the total must fall by exactly the fees that landed in between:
//!
//! `sum(before) - sum(after) == sum(ledger Landed.fee for before.slot < slot <= after.slot)`
//!
//! Every landed fee is also re-read from the chain (raw block, or getTransaction for
//! funding/sweep transfers) and compared with the ledger. Any difference fails loudly.
//! Locked rent (what sits in payers and the sink) is reported separately and is not spend.

use crate::collect::{self, Collector, Pending};
use crate::keys::NamedKeypair;
use crate::ledger::{Entry, Ledger, Purpose};
use crate::live::LiveAuth;
use crate::rpc::Rpc;
use crate::txbuild;
use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use solana_pubkey::Pubkey;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Master,
    Payer,
    Sink,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceSnapshot {
    pub ts: String,
    pub cluster: String,
    /// `context.slot` of the single getMultipleAccounts call (confirmed commitment).
    pub slot: u64,
    pub accounts: BTreeMap<String, (Role, u64)>,
}

impl BalanceSnapshot {
    pub fn total(&self) -> u64 {
        self.accounts.values().map(|(_, b)| b).sum()
    }
    pub fn locked(&self) -> u64 {
        self.accounts
            .values()
            .filter(|(r, _)| *r != Role::Master)
            .map(|(_, b)| b)
            .sum()
    }
}

pub async fn snapshot(
    rpc: &Rpc,
    cluster: &str,
    accounts: &[(Pubkey, Role)],
) -> Result<BalanceSnapshot> {
    ensure!(
        accounts.len() <= 100,
        "snapshot needs one getMultipleAccounts call (<= 100 accounts) to be consistent"
    );
    let keys: Vec<String> = accounts.iter().map(|(p, _)| p.to_string()).collect();
    let v = rpc
        .call(
            "getMultipleAccounts",
            json!([keys, {"commitment": "confirmed", "encoding": "base64",
                          "dataSlice": {"offset": 0, "length": 0}}]),
        )
        .await?;
    let slot = v["context"]["slot"].as_u64().context("context.slot")?;
    let vals = v["value"].as_array().context("value")?;
    ensure!(vals.len() == keys.len(), "getMultipleAccounts length");
    Ok(BalanceSnapshot {
        ts: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        cluster: cluster.into(),
        slot,
        accounts: keys
            .into_iter()
            .zip(accounts)
            .zip(vals)
            .map(|((k, (_, r)), a)| (k, (*r, a["lamports"].as_u64().unwrap_or(0))))
            .collect(),
    })
}

pub fn save_snapshot(data_root: &Path, s: &BalanceSnapshot) -> Result<PathBuf> {
    let dir = data_root.join("snapshots");
    std::fs::create_dir_all(&dir)?;
    let p = dir.join(format!(
        "balances_{}_slot{}.json",
        s.ts.replace([':', '-'], ""),
        s.slot
    ));
    std::fs::write(&p, serde_json::to_vec_pretty(s)?)?;
    Ok(p)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Transfer {
    pub from: String,
    pub to: String,
    pub lamports: u64,
    pub fee: u64,
    /// Account that pays `fee` and co-signs when it is not `from`; `None` means `from` pays.
    pub fee_payer: Option<String>,
}

/// Top payers up to `payer_target` and the sink up to `rent_min`, from the master.
pub fn plan_funding(
    snap: &BalanceSnapshot,
    master: &str,
    payer_target: u64,
    rent_min: u64,
    fee: u64,
) -> Result<Vec<Transfer>> {
    let mut out = vec![];
    for (k, (role, bal)) in &snap.accounts {
        let target = match role {
            Role::Payer => payer_target,
            Role::Sink => rent_min,
            Role::Master => continue,
        };
        if *bal < target {
            out.push(Transfer {
                from: master.into(),
                to: k.clone(),
                lamports: target - bal,
                fee,
                fee_payer: None,
            });
        }
    }
    let master_bal = snap.accounts.get(master).map(|x| x.1).unwrap_or(0);
    let need: u64 = out.iter().map(|t| t.lamports + t.fee).sum();
    ensure!(
        master_bal >= need + rent_min,
        "master has {master_bal} lamports, needs {need} + rent-exempt minimum {rent_min}"
    );
    Ok(out)
}

/// Return everything in payers and the sink to the master. Each account pays its own fee and
/// ends at 0 lamports (closed). If `bal - fee` is below `rent_min`, the master pays the fee
/// (and co-signs) and the account's full balance is transferred. Accounts holding no more
/// than the fee are reported as dust. `fee` is the fee of a one-signature transfer; a
/// master-paid transfer has two signatures and is planned (and reserved) at twice that.
pub fn plan_sweep(
    snap: &BalanceSnapshot,
    master: &str,
    fee: u64,
    rent_min: u64,
    busy: &std::collections::BTreeSet<String>,
) -> (Vec<Transfer>, Vec<String>) {
    let mut out = vec![];
    let mut notes = vec![];
    for (k, (role, bal)) in &snap.accounts {
        if *role == Role::Master || *bal == 0 {
            continue;
        }
        if busy.contains(k) {
            notes.push(format!("{k}: has an unresolved transaction, not swept"));
        } else if *bal <= fee {
            notes.push(format!(
                "{k}: {bal} lamports of dust (<= fee {fee}), not swept"
            ));
        } else if bal - fee < rent_min {
            out.push(Transfer {
                from: k.clone(),
                to: master.into(),
                lamports: *bal,
                // two signatures (master and the account), each charged the base fee
                fee: fee * 2,
                fee_payer: Some(master.into()),
            });
        } else {
            out.push(Transfer {
                from: k.clone(),
                to: master.into(),
                lamports: bal - fee,
                fee,
                fee_payer: None,
            });
        }
    }
    (out, notes)
}

/// Fee of a plain transfer, from the cluster (never hardcoded).
pub async fn transfer_fee(rpc: &Rpc, from: &NamedKeypair, to: &Pubkey) -> Result<u64> {
    let bh = rpc.latest_blockhash().await?;
    let t = txbuild::build_transfer(
        &from.keypair,
        to,
        1,
        &crate::runner::parse_hash(&bh.blockhash)?,
    );
    let b64 = base64::engine::general_purpose::STANDARD.encode(&t.message);
    rpc.fee_for_message(&b64)
        .await?
        .context("getFeeForMessage returned null")
}

/// Send planned transfers (live only), each reserved in the ledger first, then wait until
/// every one is resolved. Funding and sweep go through RPC; they are not measurements.
pub async fn execute_transfers(
    rpc: &Rpc,
    auth: &LiveAuth,
    ledger: &mut Ledger,
    data_root: &Path,
    signers: &BTreeMap<String, &NamedKeypair>,
    transfers: &[Transfer],
    purpose: Purpose,
) -> Result<Vec<String>> {
    let mut collector = Collector::new(rpc.clone(), data_root, None);
    let mut sigs = vec![];
    for t in transfers {
        let from = signers
            .get(&t.from)
            .with_context(|| format!("no keypair for {}", t.from))?;
        let to: Pubkey = t.to.parse().context("destination pubkey")?;
        let bh = rpc.latest_blockhash().await?;
        let hash = crate::runner::parse_hash(&bh.blockhash)?;
        let paid_by = t.fee_payer.as_deref().unwrap_or(&t.from);
        let tx = match &t.fee_payer {
            None => txbuild::build_transfer(&from.keypair, &to, t.lamports, &hash),
            Some(fp) => {
                let fp_kp = signers
                    .get(fp)
                    .with_context(|| format!("no keypair for fee payer {fp}"))?;
                txbuild::build_transfer_with_fee_payer(
                    &fp_kp.keypair,
                    &from.keypair,
                    &to,
                    t.lamports,
                    &hash,
                )
            }
        };
        let sig = tx.signature.to_string();
        ledger.reserve_paid_by(
            &sig,
            t.fee,
            bh.last_valid_block_height,
            purpose,
            None,
            Some(paid_by),
        )?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&tx.wire);
        if let Err(e) = rpc.send_transaction(auth, &b64).await {
            tracing::warn!(%sig, error = %format!("{e:#}"), "transfer send failed; reservation stays until expiry is proven");
        }
        collector.add(Pending {
            sig: sig.clone(),
            last_valid_block_height: bh.last_valid_block_height,
            sent: None,
        });
        sigs.push(sig);
    }
    collector
        .drain(Some(ledger), Duration::from_secs(180))
        .await?;
    ensure!(
        collector.pending_len() == 0,
        "{} transfers still unresolved after 180 s; run `sender collect` later",
        collector.pending_len()
    );
    Ok(sigs)
}

#[derive(Debug, Clone, Serialize)]
pub struct ReconcileReport {
    pub before_slot: u64,
    pub after_slot: u64,
    pub before_total: u64,
    pub after_total: u64,
    pub wallet_delta: i128,
    pub ledger_fees: u64,
    pub landed_in_range: usize,
    pub fees_checked_onchain: usize,
    pub open_reservations: usize,
    pub locked_rent_after: u64,
    pub problems: Vec<String>,
    pub ok: bool,
}

/// Pure part of reconciliation: wallet delta vs ledger fees (on-chain fee checks are added
/// by the caller as `problems`).
pub fn reconcile_math(
    before: &BalanceSnapshot,
    after: &BalanceSnapshot,
    entries: &[Entry],
) -> ReconcileReport {
    let mut problems = vec![];
    if before.accounts.keys().ne(after.accounts.keys()) {
        problems.push("the two snapshots cover different wallets".into());
    }
    if before.cluster != after.cluster {
        problems.push("the two snapshots are from different clusters".into());
    }
    let mut open = std::collections::BTreeSet::new();
    let mut fees = 0u64;
    let mut landed = 0usize;
    for e in entries {
        match e {
            Entry::Reserve { sig, .. } => {
                open.insert(sig.clone());
            }
            Entry::Landed { sig, fee, slot, .. } => {
                open.remove(sig);
                if *slot > before.slot && *slot <= after.slot {
                    fees += fee;
                    landed += 1;
                } else if *slot > after.slot {
                    problems.push(format!(
                        "{sig} landed at {slot}, after the closing snapshot"
                    ));
                }
            }
            Entry::Expired { sig, .. } => {
                open.remove(sig);
            }
            _ => {}
        }
    }
    if !open.is_empty() {
        problems.push(format!(
            "{} reservations unresolved: run `sender collect` first",
            open.len()
        ));
    }
    let delta = i128::from(before.total()) - i128::from(after.total());
    if delta != i128::from(fees) {
        problems.push(format!(
            "wallets fell by {delta} lamports but the ledger records {fees} in fees \
             (external transfer, or an unrecorded tx?)"
        ));
    }
    ReconcileReport {
        before_slot: before.slot,
        after_slot: after.slot,
        before_total: before.total(),
        after_total: after.total(),
        wallet_delta: delta,
        ledger_fees: fees,
        landed_in_range: landed,
        fees_checked_onchain: 0,
        open_reservations: open.len(),
        locked_rent_after: after.locked(),
        ok: problems.is_empty(),
        problems,
    }
}

fn fee_from_raw_block(data_root: &Path, slot: u64, sig: &str) -> Option<u64> {
    let f = std::fs::File::open(collect::block_path(data_root, slot)).ok()?;
    let v: Value = serde_json::from_reader(flate2::read::GzDecoder::new(f)).ok()?;
    collect::locate(&v, sig).map(|l| l.fee)
}

/// Full reconciliation: math + every landed fee in range re-read from chain data.
pub async fn reconcile(
    rpc: &Rpc,
    data_root: &Path,
    ledger_path: &Path,
    before: &BalanceSnapshot,
    after: &BalanceSnapshot,
) -> Result<ReconcileReport> {
    let entries = Ledger::read_entries(ledger_path)?;
    let mut r = reconcile_math(before, after, &entries);
    for e in &entries {
        let Entry::Landed { sig, fee, slot, .. } = e else {
            continue;
        };
        if *slot <= before.slot || *slot > after.slot {
            continue;
        }
        let onchain = match fee_from_raw_block(data_root, *slot, sig) {
            Some(f) => Some(f),
            None => rpc
                .transaction(sig)
                .await?
                .and_then(|t| t["meta"]["fee"].as_u64()),
        };
        match onchain {
            Some(f) if f == *fee => r.fees_checked_onchain += 1,
            Some(f) => r
                .problems
                .push(format!("{sig}: ledger fee {fee} != on-chain fee {f}")),
            None => r.problems.push(format!("{sig}: not found on chain")),
        }
    }
    r.ok = r.problems.is_empty();
    Ok(r)
}

pub fn load_snapshot(path: &Path) -> Result<BalanceSnapshot> {
    let s = std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))?;
    Ok(serde_json::from_str(&s)?)
}

pub fn fail_if_not_ok(r: &ReconcileReport) -> Result<()> {
    if !r.ok {
        bail!("RECONCILIATION FAILED: {}", r.problems.join("; "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(slot: u64, accts: &[(&str, Role, u64)]) -> BalanceSnapshot {
        BalanceSnapshot {
            ts: "t".into(),
            cluster: "mainnet".into(),
            slot,
            accounts: accts
                .iter()
                .map(|(k, r, b)| ((*k).to_string(), (*r, *b)))
                .collect(),
        }
    }

    fn reserve(sig: &str) -> Entry {
        Entry::Reserve {
            run_id: "r".into(),
            sig: sig.into(),
            amount: 10_000,
            last_valid_block_height: 1,
            purpose: Purpose::Measure,
            window_id: None,
            payer: None,
            ts: "t".into(),
        }
    }

    fn landed(sig: &str, fee: u64, slot: u64) -> Entry {
        Entry::Landed {
            sig: sig.into(),
            fee,
            slot,
            ts: "t".into(),
        }
    }

    #[test]
    fn funding_tops_up_and_checks_master() {
        let s = snap(
            1,
            &[
                ("M", Role::Master, 10_000_000),
                ("P1", Role::Payer, 0),
                ("P2", Role::Payer, 700_000),
                ("P3", Role::Payer, 2_000_000),
                ("S", Role::Sink, 0),
            ],
        );
        let t = plan_funding(&s, "M", 1_000_000, 650_240, 5_000).unwrap();
        let got: Vec<(&str, u64)> = t.iter().map(|t| (t.to.as_str(), t.lamports)).collect();
        assert_eq!(
            got,
            vec![("P1", 1_000_000), ("P2", 300_000), ("S", 650_240)]
        );
        let poor = snap(1, &[("M", Role::Master, 1_000_000), ("P1", Role::Payer, 0)]);
        assert!(plan_funding(&poor, "M", 1_000_000, 650_240, 5_000).is_err());
    }

    #[test]
    fn sweep_closes_accounts_and_reports_dust_and_busy() {
        let s = snap(
            1,
            &[
                ("M", Role::Master, 1),
                ("P1", Role::Payer, 1_000_000),
                ("P2", Role::Payer, 4_000),
                ("P3", Role::Payer, 900_000),
                ("S", Role::Sink, 650_245),
            ],
        );
        let busy = std::collections::BTreeSet::from(["P3".to_string()]);
        let (t, notes) = plan_sweep(&s, "M", 5_000, 650_240, &busy);
        let got: Vec<(&str, u64, Option<&str>)> = t
            .iter()
            .map(|t| (t.from.as_str(), t.lamports, t.fee_payer.as_deref()))
            .collect();
        // S would keep 645_245 < rent_min after its own fee: master pays, S sends all 650_245
        assert_eq!(got, vec![("P1", 995_000, None), ("S", 650_245, Some("M"))]);
        assert_eq!(notes.len(), 2);
        let fees: Vec<u64> = t.iter().map(|t| t.fee).collect();
        assert_eq!(fees, vec![5_000, 10_000], "two signatures cost two base fees");
    }

    #[test]
    fn sweep_fee_payer_switches_exactly_below_rent_min() {
        let s = snap(
            1,
            &[
                ("M", Role::Master, 1),
                ("A", Role::Payer, 655_239), // bal - fee = 650_239 < 650_240
                ("B", Role::Payer, 655_240), // bal - fee = 650_240 == rent_min
            ],
        );
        let (t, _) = plan_sweep(&s, "M", 5_000, 650_240, &Default::default());
        assert_eq!(t[0].fee_payer.as_deref(), Some("M"));
        assert_eq!(t[0].lamports, 655_239);
        assert_eq!(t[1].fee_payer, None);
        assert_eq!(t[1].lamports, 650_240);
    }

    #[test]
    fn reconcile_counts_fees_by_slot_and_catches_mismatch() {
        let before = snap(
            100,
            &[("M", Role::Master, 1_000_000), ("P", Role::Payer, 700_000)],
        );
        // 5_000 + 5_003 landed in range; a fee at slot 90 predates the snapshot
        let after_ok = snap(
            200,
            &[("M", Role::Master, 1_000_000), ("P", Role::Payer, 689_997)],
        );
        let entries = vec![
            reserve("old"),
            landed("old", 5_000, 90),
            reserve("a"),
            landed("a", 5_000, 150),
            reserve("b"),
            landed("b", 5_003, 160),
        ];
        let r = reconcile_math(&before, &after_ok, &entries);
        assert!(r.ok, "{:?}", r.problems);
        assert_eq!((r.ledger_fees, r.landed_in_range), (10_003, 2));
        assert_eq!(r.locked_rent_after, 689_997);

        let after_bad = snap(
            200,
            &[("M", Role::Master, 999_000), ("P", Role::Payer, 689_997)],
        );
        let r = reconcile_math(&before, &after_bad, &entries);
        assert!(!r.ok);
        assert!(r.problems[0].contains("external transfer"));
        assert!(fail_if_not_ok(&r).is_err());
    }

    #[test]
    fn reconcile_refuses_open_reservations() {
        let s = snap(100, &[("M", Role::Master, 1)]);
        let r = reconcile_math(&s, &snap(200, &[("M", Role::Master, 1)]), &[reserve("x")]);
        assert!(!r.ok);
        assert_eq!(r.open_reservations, 1);
    }
}
