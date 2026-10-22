//! Collector: resolves every sent transaction as *landed* or *provably expired*, saves the raw
//! block (`<data_root>/raw/blocks/<slot>.json.gz`), finds the block index and the producer,
//! settles the spend ledger and appends one manifest line per measurement transaction.
//!
//! Expiry proof ordering matters: the finalized block height is read *before* the signature
//! statuses. If finalized height > `lastValidBlockHeight` at that moment, every block that could
//! contain the tx is already finalized, so a `null` status read afterwards (with history search)
//! proves the tx never landed and never can.

use crate::arms::Arm;
use crate::burst::SendRecord;
use crate::ledger::{ExpiryProof, Ledger, Purpose};
use crate::rpc::Rpc;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Everything known about a measurement tx at send time. Written to `sent.jsonl` right after
/// the burst, so `collect` can finish the job after a crash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentTx {
    pub sig: String,
    pub run_id: String,
    pub cluster: String,
    pub purpose: Purpose,
    pub window_id: String,
    pub identity: String,
    pub vote_account: String,
    pub stratum: String,
    /// Selection snapshot holding this validator's client label and evidence.
    pub selection_ref: Option<String>,
    pub arm: Arm,
    pub gap_us: u64,
    pub offset_ms: i64,
    pub first_slot: u64,
    pub n_slots: u64,
    pub idx: usize,
    pub send_rank: usize,
    pub payer: String,
    pub cu_limit: u32,
    pub cu_price_micro_lamports: u64,
    pub n_signatures: u64,
    /// Arm C: the shared writable account.
    pub conflict_account: Option<String>,
    /// False for the RPC delivery check (one tx per window): such txs answer "did it
    /// land", never "in which order". Ordering analysis must skip them.
    #[serde(default = "default_true")]
    pub ordering_eligible: bool,
    pub reserved_lamports: u64,
    pub blockhash: String,
    pub last_valid_block_height: u64,
    pub planned_send_unix_ns: i128,
    pub send: SendRecord,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Landed,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub status: Status,
    pub slot: Option<u64>,
    pub block_index: Option<usize>,
    pub block_tx_count: Option<usize>,
    /// From the block's single `Fee` reward entry (VERIFIED_FACTS F8).
    pub producer: Option<String>,
    pub fee_paid: Option<u64>,
    /// `meta.err` (a failed tx still lands, pays and has a position).
    pub tx_error: Option<Value>,
    pub exclusion_reason: Option<String>,
    pub resolved_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestLine {
    pub tx: SentTx,
    pub outcome: Outcome,
}

/// A transaction awaiting resolution. Funding and sweep transfers have no `sent` context.
#[derive(Debug, Clone)]
pub struct Pending {
    pub sig: String,
    pub last_valid_block_height: u64,
    pub sent: Option<SentTx>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub index: usize,
    pub tx_count: usize,
    pub fee: u64,
    pub err: Value,
}

/// Position of `sig` in `block.transactions` (= the block order we analyze).
pub fn locate(block: &Value, sig: &str) -> Option<Located> {
    let txs = block["transactions"].as_array()?;
    let index = txs
        .iter()
        .position(|t| t["transaction"]["signatures"][0].as_str() == Some(sig))?;
    let meta = &txs[index]["meta"];
    Some(Located {
        index,
        tx_count: txs.len(),
        fee: meta["fee"].as_u64()?,
        err: meta["err"].clone(),
    })
}

/// Producer identity = pubkey of the block's single `Fee` reward. `None` if absent or ambiguous.
pub fn producer(block: &Value) -> Option<String> {
    let fees: Vec<&str> = block["rewards"]
        .as_array()?
        .iter()
        .filter(|r| r["rewardType"].as_str() == Some("Fee"))
        .filter_map(|r| r["pubkey"].as_str())
        .collect();
    match fees.as_slice() {
        [one] => Some((*one).to_string()),
        _ => None,
    }
}

/// Collect-time exclusions (the analysis adds its own, e.g. "fewer than 6 of 8 in one slot").
pub fn exclusion(
    sent: &SentTx,
    status: Status,
    slot: Option<u64>,
    producer: Option<&str>,
) -> Option<String> {
    match status {
        Status::Expired if sent.send.error.is_some() => Some("send_error".into()),
        Status::Expired => Some("not_landed".into()),
        Status::Landed => match (producer, slot) {
            (None, _) => Some("producer_unknown".into()),
            (Some(p), _) if p != sent.identity => Some("producer_mismatch".into()),
            (_, Some(s)) if s < sent.first_slot || s >= sent.first_slot + sent.n_slots => {
                Some("outside_window".into())
            }
            _ => None,
        },
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

pub fn block_path(data_root: &Path, slot: u64) -> PathBuf {
    data_root
        .join("raw")
        .join("blocks")
        .join(format!("{slot}.json.gz"))
}

/// Write the raw getBlock result once (tmp + rename, so a crash never leaves half a file).
fn save_block(data_root: &Path, slot: u64, block: &Value) -> Result<()> {
    let path = block_path(data_root, slot);
    if path.exists() {
        return Ok(());
    }
    let dir = path.parent().context("block dir")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{slot}.json.gz.tmp"));
    {
        let f = std::fs::File::create(&tmp)?;
        let mut gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        serde_json::to_writer(&mut gz, block)?;
        gz.finish()?.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn append_jsonl<T: Serialize>(path: &Path, items: &[T]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut buf = Vec::new();
    for it in items {
        serde_json::to_writer(&mut buf, it)?;
        buf.push(b'\n');
    }
    f.write_all(&buf)?;
    f.sync_data()?;
    Ok(())
}

pub fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    std::fs::read_to_string(path)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| {
            serde_json::from_str(l).with_context(|| format!("{}:{}", path.display(), i + 1))
        })
        .collect()
}

/// What one pass learned from the chain.
pub struct Observation {
    pub finalized_height: u64,
    /// Signature -> `getSignatureStatuses` entry (with history search).
    pub statuses: BTreeMap<String, Value>,
    pub blocks: BTreeMap<u64, Arc<Value>>,
}

enum Resolution {
    Pending,
    Landed {
        slot: u64,
        loc: Located,
        producer: Option<String>,
    },
    Expired(ExpiryProof),
}

fn resolve(p: &Pending, obs: &Observation) -> Resolution {
    let st = obs.statuses.get(&p.sig).unwrap_or(&Value::Null);
    if st.is_null() {
        return match ExpiryProof::new(obs.finalized_height, p.last_valid_block_height, None) {
            Some(proof) => Resolution::Expired(proof),
            None => Resolution::Pending,
        };
    }
    // Processed-only statuses have no fetched block yet: stay pending.
    let Some((slot, block)) = st["slot"]
        .as_u64()
        .and_then(|s| obs.blocks.get(&s).map(|b| (s, b)))
    else {
        return Resolution::Pending;
    };
    match locate(block, &p.sig) {
        Some(loc) => Resolution::Landed {
            slot,
            loc,
            producer: producer(block),
        },
        None => {
            tracing::error!(sig = %p.sig, slot, "status points at a block that does not contain the tx");
            Resolution::Pending
        }
    }
}

pub struct Collector {
    rpc: Rpc,
    data_root: PathBuf,
    manifest: Option<PathBuf>,
    pending: Vec<Pending>,
    blocks: BTreeMap<u64, Arc<Value>>,
}

impl Collector {
    /// `manifest`: where measurement outcomes go (None = do not write a manifest).
    pub fn new(rpc: Rpc, data_root: &Path, manifest: Option<PathBuf>) -> Self {
        Self {
            rpc,
            data_root: data_root.to_path_buf(),
            manifest,
            pending: vec![],
            blocks: BTreeMap::new(),
        }
    }

    pub fn add(&mut self, p: Pending) {
        self.pending.push(p);
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    async fn block(&mut self, slot: u64) -> Result<Arc<Value>> {
        if let Some(b) = self.blocks.get(&slot) {
            return Ok(b.clone());
        }
        let b = Arc::new(self.rpc.block(slot).await?);
        save_block(&self.data_root, slot, &b)?;
        self.blocks.insert(slot, b.clone());
        while self.blocks.len() > 64 {
            self.blocks.pop_first();
        }
        Ok(b)
    }

    /// Network half of a pass: no state changes besides the block cache and raw block files,
    /// so it is safe to cancel (the runner bounds it with a timeout between windows).
    pub async fn observe(&mut self) -> Result<Observation> {
        // Order matters, see module docs.
        let finalized_height = self.rpc.block_height_at("finalized").await?;
        let sigs: Vec<String> = self.pending.iter().map(|p| p.sig.clone()).collect();
        let statuses = self.rpc.signature_statuses(&sigs, true).await?;
        let mut blocks = BTreeMap::new();
        for st in &statuses {
            let conf = st["confirmationStatus"].as_str().unwrap_or("");
            if let (Some(slot), "confirmed" | "finalized") = (st["slot"].as_u64(), conf)
                && !blocks.contains_key(&slot)
            {
                blocks.insert(slot, self.block(slot).await?);
            }
        }
        Ok(Observation {
            finalized_height,
            statuses: sigs.into_iter().zip(statuses).collect(),
            blocks,
        })
    }

    /// Synchronous half: settle the ledger, build manifest lines, drop resolved txs.
    /// Ledger errors (e.g. a fee above its reservation) are fatal by design.
    pub fn apply(
        &mut self,
        obs: &Observation,
        mut ledger: Option<&mut Ledger>,
    ) -> Result<Vec<ManifestLine>> {
        let mut keep = vec![];
        let mut lines = vec![];
        for p in std::mem::take(&mut self.pending) {
            match resolve(&p, obs) {
                Resolution::Pending => keep.push(p),
                Resolution::Landed {
                    slot,
                    loc,
                    producer,
                } => {
                    if let Some(l) = ledger.as_deref_mut()
                        && l.is_open(&p.sig)
                    {
                        l.settle_landed(&p.sig, loc.fee, slot)?;
                    }
                    if let Some(sent) = p.sent {
                        lines.push(ManifestLine {
                            outcome: Outcome {
                                status: Status::Landed,
                                slot: Some(slot),
                                block_index: Some(loc.index),
                                block_tx_count: Some(loc.tx_count),
                                exclusion_reason: exclusion(
                                    &sent,
                                    Status::Landed,
                                    Some(slot),
                                    producer.as_deref(),
                                ),
                                producer,
                                fee_paid: Some(loc.fee),
                                tx_error: (!loc.err.is_null()).then_some(loc.err),
                                resolved_at: now(),
                            },
                            tx: sent,
                        });
                    }
                }
                Resolution::Expired(proof) => {
                    if let Some(l) = ledger.as_deref_mut()
                        && l.is_open(&p.sig)
                    {
                        l.settle_expired(&p.sig, proof)?;
                    }
                    if let Some(sent) = p.sent {
                        lines.push(ManifestLine {
                            outcome: Outcome {
                                status: Status::Expired,
                                slot: None,
                                block_index: None,
                                block_tx_count: None,
                                producer: None,
                                fee_paid: None,
                                tx_error: None,
                                exclusion_reason: exclusion(&sent, Status::Expired, None, None),
                                resolved_at: now(),
                            },
                            tx: sent,
                        });
                    }
                }
            }
        }
        self.pending = keep;
        if let Some(m) = &self.manifest {
            append_jsonl(m, &lines)?;
        }
        Ok(lines)
    }

    /// One resolution pass.
    pub async fn poll(&mut self, ledger: Option<&mut Ledger>) -> Result<Vec<ManifestLine>> {
        if self.pending.is_empty() {
            return Ok(vec![]);
        }
        let obs = self.observe().await?;
        self.apply(&obs, ledger)
    }

    /// Poll until nothing is pending or `timeout` passes. Returns all resolved lines.
    pub async fn drain(
        &mut self,
        mut ledger: Option<&mut Ledger>,
        timeout: std::time::Duration,
    ) -> Result<Vec<ManifestLine>> {
        let end = tokio::time::Instant::now() + timeout;
        let mut all = vec![];
        while !self.pending.is_empty() && tokio::time::Instant::now() < end {
            all.extend(self.poll(ledger.as_deref_mut()).await?);
            if !self.pending.is_empty() {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block() -> Value {
        json!({
            "parentSlot": 99,
            "rewards": [
                {"pubkey": "Prod", "rewardType": "Fee", "lamports": 10},
                {"pubkey": "Other", "rewardType": "Rent", "lamports": 1}
            ],
            "transactions": [
                {"transaction": {"signatures": ["s0"]}, "meta": {"fee": 5000, "err": null}},
                {"transaction": {"signatures": ["s1"]}, "meta": {"fee": 5003, "err": {"InstructionError": [0, "Custom"]}}}
            ]
        })
    }

    fn sent(identity: &str, first_slot: u64, error: Option<&str>) -> SentTx {
        SentTx {
            sig: "s1".into(),
            run_id: "r".into(),
            cluster: "mainnet".into(),
            purpose: Purpose::Measure,
            window_id: "w".into(),
            identity: identity.into(),
            vote_account: "v".into(),
            stratum: "Agave".into(),
            selection_ref: None,
            arm: Arm::P,
            gap_us: 0,
            offset_ms: -100,
            first_slot,
            n_slots: 4,
            idx: 0,
            send_rank: 0,
            payer: "p".into(),
            cu_limit: 1000,
            cu_price_micro_lamports: 1,
            n_signatures: 1,
            conflict_account: None,
            ordering_eligible: true,
            reserved_lamports: 5001,
            blockhash: "h".into(),
            last_valid_block_height: 1,
            planned_send_unix_ns: 0,
            send: SendRecord {
                idx: 0,
                sig: "s1".into(),
                path: "quic".into(),
                planned_offset_us: 0,
                start_mono_ns: 0,
                end_mono_ns: 1,
                start_unix_ns: 1_700_000_000_000_000_000,
                end_unix_ns: 1_700_000_000_000_000_001,
                stream_id: Some(0),
                conn_id: Some(1),
                rtt_us: Some(1000),
                error: error.map(str::to_string),
            },
        }
    }

    #[test]
    fn locates_tx_and_reads_fee_and_error() {
        let b = block();
        let l = locate(&b, "s1").unwrap();
        assert_eq!((l.index, l.tx_count, l.fee), (1, 2, 5003));
        assert!(!l.err.is_null());
        assert!(locate(&b, "nope").is_none());
    }

    #[test]
    fn producer_needs_exactly_one_fee_reward() {
        assert_eq!(producer(&block()).as_deref(), Some("Prod"));
        let mut two = block();
        two["rewards"]
            .as_array_mut()
            .unwrap()
            .push(json!({"pubkey": "X", "rewardType": "Fee"}));
        assert_eq!(producer(&two), None);
        assert_eq!(producer(&json!({"rewards": []})), None);
    }

    #[test]
    fn exclusion_reasons() {
        let s = sent("Prod", 100, None);
        assert_eq!(exclusion(&s, Status::Landed, Some(101), Some("Prod")), None);
        assert_eq!(
            exclusion(&s, Status::Landed, Some(101), Some("Other")).as_deref(),
            Some("producer_mismatch")
        );
        assert_eq!(
            exclusion(&s, Status::Landed, Some(104), Some("Prod")).as_deref(),
            Some("outside_window")
        );
        assert_eq!(
            exclusion(&s, Status::Landed, Some(101), None).as_deref(),
            Some("producer_unknown")
        );
        assert_eq!(
            exclusion(&s, Status::Expired, None, None).as_deref(),
            Some("not_landed")
        );
        let e = sent("Prod", 100, Some("stream reset"));
        assert_eq!(
            exclusion(&e, Status::Expired, None, None).as_deref(),
            Some("send_error")
        );
    }

    fn pending(sig: &str, lvbh: u64, identity: &str) -> Pending {
        let mut tx = sent(identity, 100, None);
        tx.sig = sig.into();
        Pending {
            sig: sig.into(),
            last_valid_block_height: lvbh,
            sent: Some(tx),
        }
    }

    #[test]
    fn apply_settles_landed_expired_and_keeps_unproven() {
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::open(&dir.path().join("l.jsonl"), "r", 1_000_000).unwrap();
        for sig in ["s0", "s1", "gone", "young", "proc"] {
            ledger
                .reserve(sig, 6_000, 50, Purpose::Measure, Some("w"))
                .unwrap();
        }
        let mut c = Collector::new(
            Rpc::new("http://127.0.0.1:9"),
            dir.path(),
            Some(dir.path().join("manifest.jsonl")),
        );
        c.add(pending("s0", 50, "Prod"));
        c.add(pending("s1", 50, "Someone"));
        c.add(pending("gone", 50, "Prod"));
        c.add(pending("young", 80, "Prod"));
        c.add(pending("proc", 50, "Prod"));
        let obs = Observation {
            finalized_height: 60,
            statuses: BTreeMap::from([
                (
                    "s0".to_string(),
                    json!({"slot": 101, "confirmationStatus": "finalized"}),
                ),
                (
                    "s1".to_string(),
                    json!({"slot": 101, "confirmationStatus": "confirmed"}),
                ),
                ("gone".to_string(), Value::Null),
                ("young".to_string(), Value::Null),
                (
                    "proc".to_string(),
                    json!({"slot": 102, "confirmationStatus": "processed"}),
                ),
            ]),
            blocks: BTreeMap::from([(101, Arc::new(block()))]),
        };
        let lines = c.apply(&obs, Some(&mut ledger)).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(ledger.settled_fee("s0"), Some(5000));
        assert_eq!(ledger.settled_fee("s1"), Some(5003));
        assert_eq!(ledger.settled_fee("gone"), Some(0));
        assert!(ledger.is_open("young") && ledger.is_open("proc"));
        assert_eq!(c.pending_len(), 2);
        let s1 = lines.iter().find(|l| l.tx.sig == "s1").unwrap();
        assert_eq!(s1.outcome.block_index, Some(1));
        assert_eq!(
            s1.outcome.exclusion_reason.as_deref(),
            Some("producer_mismatch")
        );
        assert!(s1.outcome.tx_error.is_some());
        let on_disk: Vec<ManifestLine> = read_jsonl(&dir.path().join("manifest.jsonl")).unwrap();
        assert_eq!(on_disk.len(), 3);
        // a second pass over the same observation neither double-settles nor double-writes
        assert!(c.apply(&obs, Some(&mut ledger)).unwrap().is_empty());
    }

    #[test]
    fn manifest_round_trips_and_blocks_are_saved_once() {
        let dir = tempfile::tempdir().unwrap();
        let line = ManifestLine {
            tx: sent("Prod", 100, None),
            outcome: Outcome {
                status: Status::Landed,
                slot: Some(101),
                block_index: Some(1),
                block_tx_count: Some(2),
                producer: Some("Prod".into()),
                fee_paid: Some(5003),
                tx_error: None,
                exclusion_reason: None,
                resolved_at: now(),
            },
        };
        let m = dir.path().join("manifest.jsonl");
        append_jsonl(&m, &[line.clone(), line]).unwrap();
        let back: Vec<ManifestLine> = read_jsonl(&m).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].tx.send.start_unix_ns, 1_700_000_000_000_000_000);

        save_block(dir.path(), 101, &block()).unwrap();
        let p = block_path(dir.path(), 101);
        let before = std::fs::metadata(&p).unwrap().modified().unwrap();
        save_block(dir.path(), 101, &json!({"other": 1})).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), before);
        let gz = flate2::read::GzDecoder::new(std::fs::File::open(&p).unwrap());
        let v: Value = serde_json::from_reader(gz).unwrap();
        assert_eq!(v, block());
    }
}
