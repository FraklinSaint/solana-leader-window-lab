//! Where a run executed: platform id, OS and build, machine id and the sender binary's hash.
//!
//! Recorded in every run header (`run.json`) and every manifest row (`SentTx.host`). The path
//! from our machine to the leader is part of the treatment (README "Path from our backend to
//! the executing node"), so calibration and every confirmatory window must come from ONE
//! registered machine: `SLWLAB_PLATFORM` (the operator's id for it, e.g. `win-pc1`) plus the
//! machine id. [`gate_live`] refuses live calibration or measurement on mainnet from an
//! unregistered host, or from a host other than the one calibration ran on. Dry-runs are
//! recorded the same way and are exploratory wherever they ran.

use crate::ledger::Purpose;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const PLATFORM_ENV: &str = "SLWLAB_PLATFORM";
pub const UNREGISTERED: &str = "unregistered";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// Operator-assigned platform id (`SLWLAB_PLATFORM`), or "unregistered".
    pub platform: String,
    /// `std::env::consts::OS` / `ARCH`.
    pub os: String,
    pub arch: String,
    /// Windows: the full build from `ver` (e.g. 10.0.17763.6893). Linux: kernel release.
    pub os_build: String,
    /// First 16 hex chars of SHA-256(MachineGuid) on Windows, of /etc/machine-id on Linux.
    /// Identifies the machine without publishing the id itself.
    pub machine_id: String,
    /// SHA-256 of the running sender executable (Windows keeps it locked while it runs).
    pub sender_sha256: String,
}

fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

pub fn file_sha256(path: &Path) -> Result<String> {
    Ok(sha256_hex(&std::fs::read(path)?))
}

fn command_stdout(cmd: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// "Microsoft Windows [Version 10.0.17763.6893]" -> "10.0.17763.6893".
pub fn parse_ver(s: &str) -> Option<String> {
    let i = s.find("Version ")? + "Version ".len();
    let rest = &s[i..];
    Some(
        rest[..rest.find(']').unwrap_or(rest.len())]
            .trim()
            .to_string(),
    )
}

/// `reg query ... /v MachineGuid` output -> the GUID.
pub fn parse_machine_guid(s: &str) -> Option<String> {
    s.lines()
        .find(|l| l.contains("MachineGuid"))
        .and_then(|l| l.split_whitespace().last())
        .map(str::to_string)
}

fn os_build() -> String {
    if cfg!(windows) {
        return command_stdout("cmd", &["/C", "ver"])
            .and_then(|s| parse_ver(&s))
            .unwrap_or_else(|| "unknown".into());
    }
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

fn machine_id() -> String {
    let raw = if cfg!(windows) {
        command_stdout(
            "reg",
            &[
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
            ],
        )
        .and_then(|s| parse_machine_guid(&s))
    } else {
        std::fs::read_to_string("/etc/machine-id")
            .ok()
            .map(|s| s.trim().to_string())
    };
    raw.filter(|s| !s.is_empty())
        .map(|s| sha256_hex(s.as_bytes())[..16].to_string())
        .unwrap_or_else(|| "unknown".into())
}

impl HostInfo {
    pub fn detect() -> Self {
        let sender_sha256 = std::env::current_exe()
            .ok()
            .and_then(|p| file_sha256(&p).ok())
            .unwrap_or_else(|| "unknown".into());
        Self {
            platform: std::env::var(PLATFORM_ENV)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| UNREGISTERED.into()),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            os_build: os_build(),
            machine_id: machine_id(),
            sender_sha256,
        }
    }

    /// "Same platform": platform id, OS and machine. The OS build and the binary hash are
    /// recorded and reported, not gated (a security update must not void calibration).
    pub fn same_platform(&self, other: &HostInfo) -> bool {
        self.platform == other.platform
            && self.os == other.os
            && self.machine_id == other.machine_id
    }
}

/// Live calibration and measurement on mainnet run only on a registered platform with a known
/// machine id, and only on the machine that every recorded calibration row came from
/// (`calibration`; `None` = a row without a host record). Measurement needs calibration rows.
pub fn gate_live(
    here: &HostInfo,
    purpose: Purpose,
    mainnet: bool,
    calibration: &[Option<HostInfo>],
) -> Result<()> {
    if !mainnet || !matches!(purpose, Purpose::Measure | Purpose::Calibrate) {
        return Ok(());
    }
    if here.platform == UNREGISTERED || here.machine_id == "unknown" {
        bail!(
            "live {purpose:?} on mainnet needs a registered platform: set {PLATFORM_ENV} \
             (README: Platform); this host is platform={} machine_id={}",
            here.platform,
            here.machine_id
        );
    }
    for c in calibration {
        match c {
            Some(c) if here.same_platform(c) => {}
            Some(c) => bail!(
                "calibration rows exist from {}/{}/{}; this host is {}/{}/{}. Calibration and \
                 confirmatory windows must come from one machine",
                c.platform,
                c.os,
                c.machine_id,
                here.platform,
                here.os,
                here.machine_id
            ),
            None => bail!("a calibration row has no host record; its platform is unknown"),
        }
    }
    if purpose == Purpose::Measure && calibration.is_empty() {
        bail!("no calibration rows on this platform yet: calibrate here first");
    }
    Ok(())
}

/// Host records of every row in a calibration manifest (missing file = no rows).
pub fn calibration_hosts(manifest: &Path) -> Result<Vec<Option<HostInfo>>> {
    let Ok(text) = std::fs::read_to_string(manifest) else {
        return Ok(vec![]);
    };
    let mut out = vec![];
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line)?;
        out.push(serde_json::from_value(v["tx"]["host"].clone()).ok());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(platform: &str, machine: &str) -> HostInfo {
        HostInfo {
            platform: platform.into(),
            os: "windows".into(),
            arch: "x86_64".into(),
            os_build: "10.0.17763.6893".into(),
            machine_id: machine.into(),
            sender_sha256: "ab".repeat(32),
        }
    }

    #[test]
    fn unregistered_hosts_cannot_calibrate_or_measure() {
        for here in [h(UNREGISTERED, "0123456789abcdef"), h("win-pc1", "unknown")] {
            for p in [Purpose::Calibrate, Purpose::Measure] {
                assert!(gate_live(&here, p, true, &[]).is_err());
            }
            // funding, sweeps and non-mainnet clusters are not gated
            assert!(gate_live(&here, Purpose::Fund, true, &[]).is_ok());
            assert!(gate_live(&here, Purpose::Calibrate, false, &[]).is_ok());
        }
    }

    #[test]
    fn measure_needs_calibration_from_the_same_machine() {
        let pc = h("win-pc1", "0123456789abcdef");
        assert!(gate_live(&pc, Purpose::Calibrate, true, &[]).is_ok());
        assert!(gate_live(&pc, Purpose::Measure, true, &[]).is_err());
        let mut updated = pc.clone();
        updated.os_build = "10.0.17763.7000".into();
        updated.sender_sha256 = "cd".repeat(32);
        assert!(gate_live(&pc, Purpose::Measure, true, &[Some(updated)]).is_ok());
        let other_machine = h("win-pc1", "fedcba9876543210");
        assert!(gate_live(&pc, Purpose::Measure, true, &[Some(other_machine.clone())]).is_err());
        assert!(gate_live(&pc, Purpose::Calibrate, true, &[Some(other_machine)]).is_err());
        assert!(gate_live(&pc, Purpose::Measure, true, &[None]).is_err());
    }

    #[test]
    fn parsers() {
        assert_eq!(
            parse_ver("\r\nMicrosoft Windows [Version 10.0.17763.6893]").as_deref(),
            Some("10.0.17763.6893")
        );
        let reg = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Cryptography\r\n    MachineGuid    REG_SZ    1b2c3d4e-0000-1111-2222-333344445555\r\n";
        assert_eq!(
            parse_machine_guid(reg).as_deref(),
            Some("1b2c3d4e-0000-1111-2222-333344445555")
        );
    }

    #[test]
    fn detect_hashes_the_binary_and_identifies_the_machine() {
        let d = HostInfo::detect();
        assert_eq!(d.sender_sha256.len(), 64);
        assert_eq!(d.os, std::env::consts::OS);
        assert_ne!(d.os_build, "unknown");
        assert_eq!(d.machine_id.len(), 16);
        assert_eq!(HostInfo::detect().machine_id, d.machine_id);
    }

    #[test]
    fn calibration_hosts_reads_rows_and_missing_records() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.jsonl");
        assert!(calibration_hosts(&p).unwrap().is_empty());
        let row =
            serde_json::json!({"tx": {"host": h("win-pc1", "0123456789abcdef")}, "outcome": {}});
        let old = serde_json::json!({"tx": {}, "outcome": {}});
        std::fs::write(&p, format!("{row}\n{old}\n")).unwrap();
        let got = calibration_hosts(&p).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got[0].is_some() && got[1].is_none());
    }
}
