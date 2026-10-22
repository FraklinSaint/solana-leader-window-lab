//! Credit meter: an *estimate* of the provider credits a run consumes, so a 24 h dry-run or a
//! calibration session cannot silently exhaust the free plan (Helius free: 1,000,000 credits
//! per month).
//!
//! Cost table (Helius docs, "Credits", checked 2026-10-22): every standard RPC method costs
//! 1 credit, including getBlock, getTransaction, getSignatureStatuses, simulateTransaction
//! and sendTransaction (staked connections). getProgramAccounts costs 10 (we never call it).
//! WebSocket streaming costs 2 credits per 0.1 MB of uncompressed payload; we count the
//! bytes of every received text frame.
//!
//! The provider's dashboard is the authority: other users of the same key, and requests
//! that fail before reaching us, are not counted here. Totals per run go to the run log and
//! to `data/credits/credits.jsonl` (one line per command), which also gives the
//! month-to-date estimate used for the 50% warning.

use chrono::{Datelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const FREE_PLAN_MONTHLY_CREDITS: u64 = 1_000_000;
/// Warn once month-to-date credits reach this share of the monthly quota.
pub const WARN_SHARE: f64 = 0.5;
pub const WS_CREDITS_PER_100KB: f64 = 2.0;

pub fn credits_for(method: &str) -> f64 {
    match method {
        "getProgramAccounts" => 10.0,
        _ => 1.0,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MethodCount {
    pub calls: u64,
    pub credits: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub by_method: BTreeMap<String, MethodCount>,
    pub ws_bytes: u64,
    pub ws_credits: f64,
    /// HTTP 429 / "Too many requests" responses (each retried request was also counted).
    pub rate_limited: u64,
    pub credits: f64,
}

/// One line of `data/credits/credits.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditLine {
    pub ts: String,
    pub month: String,
    pub run_id: String,
    pub endpoint: String,
    pub usage: Usage,
}

struct Inner {
    usage: Usage,
    month_before_run: f64,
    quota: u64,
    warned: bool,
}

/// Shared by every clone of an `Rpc` and by the slot feed.
#[derive(Clone)]
pub struct CreditMeter(Arc<Mutex<Inner>>);

pub fn month_key() -> String {
    let now = Utc::now();
    format!("{:04}-{:02}", now.year(), now.month())
}

pub fn ledger_path(data_dir: &Path) -> PathBuf {
    data_dir.join("credits").join("credits.jsonl")
}

/// Month-to-date credits recorded in the credits ledger for `month`.
pub fn month_to_date(path: &Path, month: &str) -> f64 {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0.0;
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<CreditLine>(l).ok())
        .filter(|l| l.month == month)
        .map(|l| l.usage.credits)
        .sum()
}

impl Default for CreditMeter {
    fn default() -> Self {
        Self::new(0.0, FREE_PLAN_MONTHLY_CREDITS)
    }
}

impl CreditMeter {
    pub fn new(month_before_run: f64, quota: u64) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            usage: Usage::default(),
            month_before_run,
            quota,
            warned: false,
        })))
    }

    fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        let mut g = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let r = f(&mut g);
        let mtd = g.month_before_run + g.usage.credits;
        let limit = g.quota as f64 * WARN_SHARE;
        if !g.warned && mtd >= limit {
            g.warned = true;
            tracing::warn!(
                month_to_date_credits = mtd,
                quota = g.quota,
                "estimated provider credits reached 50% of the monthly free quota"
            );
        }
        r
    }

    pub fn http_call(&self, method: &str) {
        let c = credits_for(method);
        self.with(|g| {
            let m = g.usage.by_method.entry(method.to_string()).or_default();
            m.calls += 1;
            m.credits += c;
            g.usage.credits += c;
        });
    }

    pub fn ws_bytes(&self, n: usize) {
        let c = n as f64 / 100_000.0 * WS_CREDITS_PER_100KB;
        self.with(|g| {
            g.usage.ws_bytes += n as u64;
            g.usage.ws_credits += c;
            g.usage.credits += c;
        });
    }

    pub fn rate_limited(&self) {
        self.with(|g| g.usage.rate_limited += 1);
    }

    pub fn usage(&self) -> Usage {
        self.with(|g| g.usage.clone())
    }

    pub fn month_to_date(&self) -> f64 {
        self.with(|g| g.month_before_run + g.usage.credits)
    }

    pub fn warned(&self) -> bool {
        self.with(|g| g.warned)
    }

    /// Append this command's usage to the credits ledger (endpoint already redacted).
    pub fn record(&self, path: &Path, run_id: &str, endpoint: &str) -> anyhow::Result<()> {
        let line = CreditLine {
            ts: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            month: month_key(),
            run_id: run_id.to_string(),
            endpoint: crate::redact::redact(endpoint).into_owned(),
            usage: self.usage(),
        };
        crate::collect::append_jsonl(path, std::slice::from_ref(&line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_calls_ws_bytes_and_warns_at_half_quota() {
        let m = CreditMeter::new(499_990.0, 1_000_000);
        for _ in 0..3 {
            m.http_call("getBlock");
        }
        m.http_call("getProgramAccounts");
        m.ws_bytes(250_000); // 2.5 x 0.1 MB = 5 credits
        let u = m.usage();
        assert_eq!(u.by_method["getBlock"].calls, 3);
        assert_eq!(u.by_method["getProgramAccounts"].credits, 10.0);
        assert!((u.ws_credits - 5.0).abs() < 1e-9);
        assert!((u.credits - 18.0).abs() < 1e-9);
        assert!(m.warned());
        assert!(!CreditMeter::new(0.0, 1_000_000).warned());
    }

    #[test]
    fn month_to_date_sums_only_this_month() {
        let dir = tempfile::tempdir().unwrap();
        let p = ledger_path(dir.path());
        let m = CreditMeter::default();
        m.http_call("getSlot");
        m.http_call("getSlot");
        m.record(
            &p,
            "r1",
            &format!("https://h/?api-key={}", "0123456789abcdef"),
        )
        .unwrap();
        m.record(&p, "r2", "https://h/").unwrap();
        let mut old: CreditLine =
            serde_json::from_str(std::fs::read_to_string(&p).unwrap().lines().next().unwrap())
                .unwrap();
        old.month = "1999-01".into();
        crate::collect::append_jsonl(&p, &[old]).unwrap();
        assert_eq!(month_to_date(&p, &month_key()), 4.0);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("0123456789abcdef"));
    }
}
