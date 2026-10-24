//! Keypair files (solana-keygen JSON format) under `./secrets`. Secret bytes are never
//! logged or printed: the only types that leave this module expose the public key.

use anyhow::{Context, Result, bail};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct NamedKeypair {
    pub name: String,
    pub keypair: Keypair,
}

impl std::fmt::Debug for NamedKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NamedKeypair({}, {})", self.name, self.keypair.pubkey())
    }
}

impl NamedKeypair {
    pub fn pubkey(&self) -> Pubkey {
        self.keypair.pubkey()
    }
}

pub fn read_keypair(path: &Path) -> Result<Keypair> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading keypair {}", path.display()))?;
    let bytes: Vec<u8> = serde_json::from_str(&text)
        .with_context(|| format!("{}: not a JSON byte array", path.display()))?;
    if bytes.len() != 64 {
        bail!("{}: expected 64 bytes, got {}", path.display(), bytes.len());
    }
    Keypair::try_from(bytes.as_slice()).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

/// File in a secrets dir naming the one cluster its keys belong to (`mainnet`, `devnet`,
/// `local-<genesis prefix>`). Git-ignored like everything else there.
pub const CLUSTER_MARKER: &str = "CLUSTER";

/// Folder names that state a cluster by themselves (`secrets/devnet`).
const CLUSTER_DIR_NAMES: [&str; 4] = ["mainnet", "devnet", "testnet", "localnet"];

fn name_matches(name: &str, cluster: &str) -> bool {
    name == cluster || (name == "localnet" && cluster.starts_with("local-"))
}

fn has_keys(dir: &Path) -> bool {
    let json_in = |d: &Path| {
        std::fs::read_dir(d).is_ok_and(|it| {
            it.flatten()
                .any(|e| e.path().extension().is_some_and(|x| x == "json"))
        })
    };
    dir.join("master.json").exists()
        || dir.join("sink.json").exists()
        || json_in(&dir.join("payers"))
}

/// Guard against using one cluster's keys on another (devnet keys against mainnet, or the
/// reverse). Called by every command once the cluster is known from the genesis hash.
/// - marker present: it must name `cluster`;
/// - no marker, no keys: nothing to guard;
/// - no marker, keys, folder named after a cluster: that name must match, and the marker is
///   then written (binds a folder made before this guard existed);
/// - no marker, keys, any other name: refused until the operator writes the marker.
pub fn guard_secrets_cluster(dir: &Path, cluster: &str) -> Result<()> {
    let marker = dir.join(CLUSTER_MARKER);
    if let Ok(text) = std::fs::read_to_string(&marker) {
        let bound = text.trim();
        if bound != cluster {
            bail!(
                "secrets dir {} is bound to {bound} ({}), but the endpoint is {cluster}: \
                 use the secrets dir of this cluster (--secrets-dir) or the other env file",
                dir.display(),
                marker.display()
            );
        }
        return Ok(());
    }
    if !has_keys(dir) {
        return Ok(());
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if CLUSTER_DIR_NAMES.contains(&name.as_str()) {
        if !name_matches(&name, cluster) {
            bail!(
                "secrets dir {} is named {name}, but the endpoint is {cluster}: refusing to use \
                 {name} keys on {cluster}",
                dir.display()
            );
        }
        return bind_secrets_cluster(dir, cluster);
    }
    bail!(
        "secrets dir {} holds keys but has no CLUSTER marker, so its cluster is unknown. If these \
         keys are for {cluster}, write the marker yourself: Set-Content -NoNewline -Path '{}' -Value {cluster}",
        dir.display(),
        marker.display()
    )
}

/// Bind `dir` to `cluster` (creates the dir); refuses a dir already bound elsewhere.
pub fn bind_secrets_cluster(dir: &Path, cluster: &str) -> Result<()> {
    let marker = dir.join(CLUSTER_MARKER);
    if marker.exists() {
        return guard_secrets_cluster(dir, cluster);
    }
    std::fs::create_dir_all(dir)?;
    std::fs::write(&marker, cluster).with_context(|| marker.display().to_string())
}

/// Writes a new keypair file; refuses to overwrite.
pub fn write_new_keypair(path: &Path, kp: &Keypair) -> Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| format!("creating {} (refusing to overwrite)", path.display()))?;
    let bytes = kp.to_bytes();
    f.write_all(serde_json::to_string(&bytes.to_vec())?.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

pub fn payer_path(dir: &Path, i: usize) -> PathBuf {
    dir.join(format!("payer_{i:03}.json"))
}

/// Generate payers `existing..count` (keeps existing files).
pub fn init_payers(dir: &Path, count: usize) -> Result<Vec<Pubkey>> {
    std::fs::create_dir_all(dir)?;
    let mut out = vec![];
    for i in 0..count {
        let p = payer_path(dir, i);
        if p.exists() {
            out.push(read_keypair(&p)?.pubkey());
        } else {
            let kp = Keypair::new();
            write_new_keypair(&p, &kp)?;
            out.push(kp.pubkey());
        }
    }
    Ok(out)
}

pub fn load_payers(dir: &Path) -> Result<Vec<NamedKeypair>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("payer_") && n.ends_with(".json"))
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            Ok(NamedKeypair {
                name: p.file_stem().unwrap_or_default().to_string_lossy().into(),
                keypair: read_keypair(&p)?,
            })
        })
        .collect()
}

/// Dry-run without secrets: ephemeral payers that exist only in memory.
pub fn ephemeral_payers(n: usize) -> Vec<NamedKeypair> {
    (0..n)
        .map(|i| NamedKeypair {
            name: format!("ephemeral_{i:03}"),
            keypair: Keypair::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_no_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let pks = init_payers(d.path(), 3).unwrap();
        assert_eq!(init_payers(d.path(), 3).unwrap(), pks); // idempotent
        let loaded = load_payers(d.path()).unwrap();
        assert_eq!(loaded.iter().map(|k| k.pubkey()).collect::<Vec<_>>(), pks);
        let kp = Keypair::new();
        assert!(write_new_keypair(&payer_path(d.path(), 0), &kp).is_err());
    }

    #[test]
    fn debug_never_contains_secret() {
        let k = &ephemeral_payers(1)[0];
        let dbg = format!("{k:?}");
        let secret = serde_json::to_string(&k.keypair.to_bytes().to_vec()).unwrap();
        assert!(!dbg.contains(&secret[1..20]));
        assert!(dbg.contains(&k.pubkey().to_string()));
    }

    fn with_keys(dir: &std::path::Path) {
        super::init_payers(&dir.join("payers"), 1).unwrap();
    }

    #[test]
    fn marker_binds_a_secrets_dir_to_one_cluster() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("keys");
        // no keys yet: nothing to guard, nothing written
        super::guard_secrets_cluster(&d, "mainnet").unwrap();
        assert!(!d.join(super::CLUSTER_MARKER).exists());
        super::bind_secrets_cluster(&d, "devnet").unwrap();
        with_keys(&d);
        super::guard_secrets_cluster(&d, "devnet").unwrap();
        let e = super::guard_secrets_cluster(&d, "mainnet")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("bound to devnet") && e.contains("mainnet"),
            "{e}"
        );
        // binding again to the other cluster is refused too
        assert!(super::bind_secrets_cluster(&d, "mainnet").is_err());
        // and the reverse direction
        let m = t.path().join("main");
        super::bind_secrets_cluster(&m, "mainnet").unwrap();
        with_keys(&m);
        assert!(super::guard_secrets_cluster(&m, "devnet").is_err());
        super::guard_secrets_cluster(&m, "mainnet").unwrap();
    }

    #[test]
    fn unbound_dir_with_keys_is_bound_by_its_name_or_refused() {
        let t = tempfile::tempdir().unwrap();
        // secrets/devnet with keys and no marker (made before the guard existed)
        let dev = t.path().join("devnet");
        with_keys(&dev);
        let e = super::guard_secrets_cluster(&dev, "mainnet")
            .unwrap_err()
            .to_string();
        assert!(e.contains("devnet"), "{e}");
        assert!(
            !dev.join(super::CLUSTER_MARKER).exists(),
            "a refusal writes nothing"
        );
        super::guard_secrets_cluster(&dev, "devnet").unwrap();
        assert_eq!(
            std::fs::read_to_string(dev.join(super::CLUSTER_MARKER))
                .unwrap()
                .trim(),
            "devnet"
        );
        // a localnet folder matches any local test validator
        let local = t.path().join("localnet");
        with_keys(&local);
        super::guard_secrets_cluster(&local, "local-4uhcVJyU").unwrap();
        // a neutral name with keys and no marker: the operator must say which cluster
        let plain = t.path().join("secrets");
        with_keys(&plain);
        let e = super::guard_secrets_cluster(&plain, "mainnet")
            .unwrap_err()
            .to_string();
        assert!(e.contains("no CLUSTER marker"), "{e}");
    }
}
