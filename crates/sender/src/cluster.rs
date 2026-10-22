//! Which cluster an RPC endpoint serves, decided by its genesis hash (never by a flag, so a
//! devnet run cannot be mislabelled as mainnet). The Alpenglow guard and the measurement data
//! paths apply to mainnet only; every other cluster writes under `data/nonmainnet/<name>/`,
//! which the analysis never reads.

use std::path::{Path, PathBuf};

/// Verified with getGenesisHash on 2026-10-22 (api.mainnet-beta.solana.com / api.devnet.solana.com).
pub const MAINNET_GENESIS: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
pub const DEVNET_GENESIS: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cluster {
    Mainnet,
    Devnet,
    /// Anything else (solana-test-validator, private clusters), keyed by genesis hash.
    Other(String),
}

impl Cluster {
    pub fn from_genesis(hash: &str) -> Self {
        match hash {
            MAINNET_GENESIS => Cluster::Mainnet,
            DEVNET_GENESIS => Cluster::Devnet,
            other => Cluster::Other(other.to_string()),
        }
    }

    pub fn name(&self) -> String {
        match self {
            Cluster::Mainnet => "mainnet".into(),
            Cluster::Devnet => "devnet".into(),
            Cluster::Other(h) => format!("local-{}", &h[..8.min(h.len())]),
        }
    }

    pub fn is_mainnet(&self) -> bool {
        *self == Cluster::Mainnet
    }

    /// Root for everything a run writes (ledger, logs, raw blocks, manifests).
    pub fn data_root(&self, data_dir: &Path) -> PathBuf {
        if self.is_mainnet() {
            data_dir.to_path_buf()
        } else {
            data_dir.join("nonmainnet").join(self.name())
        }
    }

    /// The Alpenglow/stabilization guard for slots we plan or send into. Mainnet only:
    /// other clusters have unrelated slot numbers and are never measurement data.
    pub fn check_live_slot(&self, slot: u64, min_live_epoch: u64) -> Result<(), String> {
        if self.is_mainnet() {
            slwlab_common::guard::check_live_slot(slot, min_live_epoch).map_err(|e| e.to_string())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mainnet_is_guarded_and_writes_to_data() {
        let c = Cluster::from_genesis(MAINNET_GENESIS);
        assert!(c.is_mainnet());
        assert_eq!(c.data_root(Path::new("data")), PathBuf::from("data"));
        assert!(c.check_live_slot(1_000, 1065).is_err());
        assert!(c.check_live_slot(460_080_000, 1065).is_ok());
    }

    #[test]
    fn other_clusters_are_segregated() {
        let d = Cluster::from_genesis(DEVNET_GENESIS);
        assert_eq!(
            d.data_root(Path::new("data")),
            Path::new("data").join("nonmainnet").join("devnet")
        );
        let l = Cluster::from_genesis("4uhcVJyU9pJkvQyS88uRDiswHXSCkY3zQawwpjk2NsNY");
        assert_eq!(l.name(), "local-4uhcVJyU");
        assert!(l.check_live_slot(5, 1065).is_ok());
        assert!(!l.data_root(Path::new("data")).starts_with("data/raw"));
    }
}
