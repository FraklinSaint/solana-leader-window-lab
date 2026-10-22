//! Fails if a provider API key appears in any tracked file or in anything under `data/`
//! (run logs, manifests, snapshots, credits ledger; tracked or not yet tracked).
//! `.gz` files are decompressed first. The local `.env`, which legitimately holds the key,
//! must be git-ignored. Mirrors `scripts/check_no_keys.py` (CI secrets-check).

use slwlab_sender::redact::contains_api_key;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn read_text(p: &Path) -> Option<String> {
    let bytes = std::fs::read(p).ok()?;
    let bytes = if p.extension().is_some_and(|e| e == "gz") {
        let mut out = vec![];
        flate2::read::GzDecoder::new(&bytes[..])
            .read_to_end(&mut out)
            .ok()?;
        out
    } else {
        bytes
    };
    String::from_utf8(bytes).ok()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn no_api_key_in_tracked_files_or_data() {
    let root = repo_root();
    let ls = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(&root)
        .output();
    let mut files: Vec<PathBuf> = match ls {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(|s| root.join(s))
            .collect(),
        // No git (e.g. a source tarball): fall back to data/ only.
        _ => vec![],
    };
    walk(&root.join("data"), &mut files);
    files.sort();
    files.dedup();
    let bad: Vec<String> = files
        .iter()
        .filter(|p| read_text(p).is_some_and(|t| contains_api_key(&t)))
        .map(|p| p.strip_prefix(&root).unwrap_or(p).display().to_string())
        .collect();
    assert!(bad.is_empty(), "API key pattern found in: {bad:?}");
}

#[test]
fn dotenv_is_ignored_by_git() {
    let root = repo_root();
    if let Ok(o) = Command::new("git")
        .args(["check-ignore", "-q", ".env"])
        .current_dir(&root)
        .output()
    {
        assert!(o.status.success(), ".env must be listed in .gitignore");
    }
}
