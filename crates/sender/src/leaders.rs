//! Leader-schedule snapshots, one per epoch (`<data_root>/snapshots/leader_schedule_epoch<E>.json.gz`).
//!
//! RPC providers serve `getLeaderSchedule` only for about the last 10 epochs (LOGBOOK
//! 2026-10-22 21:30), and an epoch lasts about 25 h. The raw schedule of every epoch we plan in
//! is therefore saved the first time we meet it, so `scripts/verify_manifest.py` can check
//! offline that each window's validator was the scheduled leader of its slots.

use crate::rpc::{EpochInfo, Rpc};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub fn snapshot_path(data_root: &Path, epoch: u64) -> PathBuf {
    data_root
        .join("snapshots")
        .join(format!("leader_schedule_epoch{epoch}.json.gz"))
}

/// Epoch boundaries derived from one `getEpochInfo` (valid after warm-up on any cluster).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochMap {
    pub epoch: u64,
    pub first_slot: u64,
    pub slots_in_epoch: u64,
}

impl EpochMap {
    pub fn from_info(i: &EpochInfo) -> Self {
        Self {
            epoch: i.epoch,
            first_slot: i.absolute_slot - i.slot_index,
            slots_in_epoch: i.slots_in_epoch,
        }
    }
    /// (epoch, first slot of that epoch) for `slot`.
    pub fn epoch_of(&self, slot: u64) -> (u64, u64) {
        let d = i128::from(slot) - i128::from(self.first_slot);
        let k = d.div_euclid(i128::from(self.slots_in_epoch));
        let epoch = (i128::from(self.epoch) + k) as u64;
        let first = (i128::from(self.first_slot) + k * i128::from(self.slots_in_epoch)) as u64;
        (epoch, first)
    }
}

/// The stored document: the raw RPC result plus what is needed to read it.
pub fn document(cluster: &str, m: &EpochMap, epoch: u64, first_slot: u64, raw: &Value) -> Value {
    json!({
        "label": "SNAPSHOT",
        "cluster": cluster,
        "method": "getLeaderSchedule",
        "params": [first_slot],
        "epoch": epoch,
        "first_slot": first_slot,
        "slots_in_epoch": m.slots_in_epoch,
        "fetched_utc": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "note": "schedule: identity -> slot indices relative to first_slot (raw RPC result)",
        "schedule": raw,
    })
}

/// Write once (tmp + rename). An existing snapshot is never overwritten.
pub fn save(data_root: &Path, epoch: u64, doc: &Value) -> Result<bool> {
    let path = snapshot_path(data_root, epoch);
    if path.exists() {
        return Ok(false);
    }
    let dir = path.parent().context("snapshot dir")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("leader_schedule_epoch{epoch}.json.gz.tmp"));
    {
        let f = std::fs::File::create(&tmp)?;
        let mut gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        serde_json::to_writer(&mut gz, doc)?;
        gz.finish()?.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(true)
}

pub fn load(data_root: &Path, epoch: u64) -> Result<Value> {
    let f = std::fs::File::open(snapshot_path(data_root, epoch))?;
    Ok(serde_json::from_reader(flate2::read::GzDecoder::new(f))?)
}

/// Save the schedule of `slot`'s epoch unless it is already on disk. Returns the epoch and
/// whether a new file was written.
pub async fn ensure(
    rpc: &Rpc,
    data_root: &Path,
    cluster: &str,
    m: &EpochMap,
    slot: u64,
) -> Result<(u64, bool)> {
    let (epoch, first) = m.epoch_of(slot);
    if snapshot_path(data_root, epoch).exists() {
        return Ok((epoch, false));
    }
    let raw = rpc.leader_schedule_raw(first).await?;
    if raw.is_null() {
        bail!("getLeaderSchedule: no schedule for epoch {epoch}");
    }
    let written = save(data_root, epoch, &document(cluster, m, epoch, first, &raw))?;
    Ok((epoch, written))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> EpochMap {
        EpochMap::from_info(&EpochInfo {
            absolute_slot: 460_188_407,
            block_height: 0,
            epoch: 1065,
            slot_index: 108_407,
            slots_in_epoch: 432_000,
        })
    }

    #[test]
    fn epoch_of_matches_mainnet_boundaries() {
        let m = map();
        assert_eq!(m.first_slot, 460_080_000);
        assert_eq!(m.epoch_of(460_080_000), (1065, 460_080_000));
        assert_eq!(m.epoch_of(460_511_999), (1065, 460_080_000));
        assert_eq!(m.epoch_of(460_512_000), (1066, 460_512_000));
        assert_eq!(m.epoch_of(459_653_000), (1064, 459_648_000));
        // mainnet has no warm-up at these slots: identical to slot / 432_000
        for s in [459_653_000u64, 460_079_999, 461_000_123] {
            assert_eq!(m.epoch_of(s).0, s / slwlab_common::guard::SLOTS_PER_EPOCH);
        }
    }

    #[test]
    fn save_is_write_once_and_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let m = map();
        let raw = json!({"Leader1": [0, 1, 2, 3]});
        let doc = document("mainnet", &m, 1065, 460_080_000, &raw);
        assert!(save(d.path(), 1065, &doc).unwrap());
        let other = document("mainnet", &m, 1065, 460_080_000, &json!({"X": [0]}));
        assert!(!save(d.path(), 1065, &other).unwrap());
        let back = load(d.path(), 1065).unwrap();
        assert_eq!(back["schedule"], raw);
        assert_eq!(back["first_slot"], 460_080_000);
        assert!(back["fetched_utc"].as_str().unwrap().ends_with('Z'));
        assert!(
            !snapshot_path(d.path(), 1065)
                .with_extension("gz.tmp")
                .exists()
        );
    }
}
