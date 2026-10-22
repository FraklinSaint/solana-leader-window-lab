//! Long unattended runs (the 24 h dry-run) must survive host sleep, network loss and slot-feed
//! stalls, and must say exactly when they were blind. A watchdog task appends every gap to
//! `<run>/gaps.jsonl`; [`hourly`] turns a set of run directories into the per-UTC-hour
//! window-start prediction-error distribution (`clock_hourly.json`).
//!
//! Gap kinds:
//! - `host_stall`: our own 1 s tick arrived more than [`HOST_STALL_NS`] late. The PC slept or
//!   hibernated, or the process was starved. After a stall longer than [`CLOCK_RESET_NS`],
//!   the slot clock is cleared, because the wall clock may have been stepped on resume. The
//!   runner then waits for fresh notifications before planning.
//! - `slot_feed`: no slot notification for more than [`FEED_STALL_NS`] (WebSocket down, or
//!   the 5 s polling fallback).
//! - `rpc_error`: an RPC call the run loop needs failed after all retries. The loop waits
//!   and continues instead of exiting.

use crate::burst::unix_ns;
use crate::collect;
use crate::feed;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const HOST_STALL_NS: i128 = 5_000_000_000;
pub const CLOCK_RESET_NS: i128 = 30_000_000_000;
pub const FEED_STALL_NS: i128 = 3_000_000_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Gap {
    pub kind: String,
    pub start_unix_ns: i128,
    pub end_unix_ns: i128,
    pub secs: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Gap {
    pub fn new(kind: &str, start: i128, end: i128, detail: Option<String>) -> Self {
        Self {
            kind: kind.into(),
            start_unix_ns: start,
            end_unix_ns: end,
            secs: (end - start) as f64 / 1e9,
            detail,
        }
    }
}

pub fn log_gap(run_dir: &Path, g: &Gap) {
    tracing::warn!(kind = %g.kind, secs = g.secs, detail = ?g.detail, "gap");
    if let Err(e) = collect::append_jsonl(&run_dir.join("gaps.jsonl"), std::slice::from_ref(g)) {
        tracing::warn!(error = %e, "could not write gaps.jsonl");
    }
}

/// Pure state machine of the watchdog, driven by (now, latest notification time).
#[derive(Debug, Default)]
pub struct Watch {
    last_tick: Option<i128>,
    feed_stall_since: Option<i128>,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Gap(Gap),
    ResetClock,
}

impl Watch {
    pub fn tick(&mut self, now: i128, latest_notification: Option<i128>) -> Vec<Event> {
        let mut out = vec![];
        if let Some(prev) = self.last_tick
            && now - prev > HOST_STALL_NS
        {
            out.push(Event::Gap(Gap::new("host_stall", prev, now, None)));
            if now - prev > CLOCK_RESET_NS {
                out.push(Event::ResetClock);
            }
        }
        self.last_tick = Some(now);
        match (self.feed_stall_since, latest_notification) {
            (None, Some(l)) if now - l > FEED_STALL_NS => self.feed_stall_since = Some(l),
            (Some(since), Some(l)) if l > since => {
                out.push(Event::Gap(Gap::new("slot_feed", since, l, None)));
                self.feed_stall_since = None;
            }
            _ => {}
        }
        out
    }

    /// Close an open feed stall at the end of a run.
    pub fn close(&mut self, now: i128) -> Option<Gap> {
        self.feed_stall_since
            .take()
            .map(|s| Gap::new("slot_feed", s, now, Some("open at end of run".into())))
    }
}

/// Spawn the watchdog for a run. Abort the handle when the run ends.
pub fn spawn_watchdog(
    run_dir: PathBuf,
    state: feed::Shared,
    clock: crate::clock::ClockParams,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut w = Watch::default();
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let latest = state
                .lock()
                .ok()
                .and_then(|s| s.first_seen.last_key_value().map(|(_, t)| *t));
            for ev in w.tick(unix_ns(), latest) {
                match ev {
                    Event::Gap(g) => log_gap(&run_dir, &g),
                    Event::ResetClock => {
                        if let Ok(mut s) = state.lock() {
                            s.clock = crate::clock::SlotClock::with_params(clock);
                        }
                        tracing::warn!("slot clock cleared after a host stall; re-warming");
                    }
                }
            }
        }
    })
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

fn hour_of(unix_ns: i128) -> String {
    chrono::DateTime::from_timestamp((unix_ns / 1_000_000_000) as i64, 0)
        .map(|t| t.format("%Y-%m-%dT%H:00:00Z").to_string())
        .unwrap_or_else(|| "invalid".into())
}

#[derive(Default)]
struct Hour {
    errs: Vec<f64>,
    unobserved: usize,
    executed: usize,
    skipped: usize,
    gap_secs: BTreeMap<String, f64>,
}

/// Per-UTC-hour window-start prediction error (prep-time prediction vs the arrival of the
/// window's first slot notification) plus window counts and gap seconds, over one or more
/// run directories (a 24 h campaign restarted after crashes spans several).
pub fn hourly(run_dirs: &[PathBuf]) -> anyhow::Result<Value> {
    let mut hours: BTreeMap<String, Hour> = BTreeMap::new();
    let mut runs = vec![];
    for d in run_dirs {
        runs.push(d.display().to_string());
        let read = |f: &str| -> Vec<Value> {
            collect::read_jsonl::<Value>(&d.join(f)).unwrap_or_default()
        };
        for c in read("clock.jsonl") {
            let Some(t) = c["predicted_prep_unix_ns"]
                .as_i64()
                .map(i128::from)
                .or_else(|| c["predicted_plan_unix_ns"].as_i64().map(i128::from))
            else {
                continue;
            };
            let h = hours.entry(hour_of(t)).or_default();
            match c["err_prep_ms"].as_f64() {
                Some(e) => h.errs.push(e),
                None => h.unobserved += 1,
            }
        }
        for w in read("windows.jsonl") {
            let Some(t) = w["window"]["send_at_unix_ns"].as_i64().map(i128::from) else {
                continue;
            };
            let h = hours.entry(hour_of(t)).or_default();
            if w["outcome"] == "skipped" {
                h.skipped += 1;
            } else {
                h.executed += 1;
            }
        }
        for g in collect::read_jsonl::<Gap>(&d.join("gaps.jsonl")).unwrap_or_default() {
            // Split gaps across hour boundaries.
            let mut s = g.start_unix_ns;
            while s < g.end_unix_ns {
                let next_hour = (s / 3_600_000_000_000 + 1) * 3_600_000_000_000;
                let e = g.end_unix_ns.min(next_hour);
                *hours
                    .entry(hour_of(s))
                    .or_default()
                    .gap_secs
                    .entry(g.kind.clone())
                    .or_default() += (e - s) as f64 / 1e9;
                s = e;
            }
        }
    }
    let rows: Vec<Value> = hours
        .into_iter()
        .map(|(hour, mut h)| {
            let n = h.errs.len();
            let mut abs: Vec<f64> = h.errs.iter().map(|e| e.abs()).collect();
            abs.sort_by(|a, b| a.total_cmp(b));
            let stats = (n > 0).then(|| {
                json!({
                    "median_abs_ms": quantile(&abs, 0.5),
                    "p90_abs_ms": quantile(&abs, 0.9),
                    "p95_abs_ms": quantile(&abs, 0.95),
                    "max_abs_ms": abs[n - 1],
                    "mean_ms": h.errs.iter().sum::<f64>() / n as f64,
                    "share_within_100ms": abs.iter().filter(|e| **e <= 100.0).count() as f64 / n as f64,
                    "share_within_425ms": abs.iter().filter(|e| **e <= 425.0).count() as f64 / n as f64,
                })
            });
            h.errs.clear();
            json!({
                "hour_utc": hour, "observed": n, "unobserved": h.unobserved,
                "windows_executed": h.executed, "windows_skipped": h.skipped,
                "gap_secs": h.gap_secs, "err": stats,
            })
        })
        .collect();
    Ok(json!({
        "label": "EXPLORATORY",
        "metric": "window-start prediction error at prep time: arrival of the window's first slot \
                   notification at this machine minus the prediction. NOT the leader's real block \
                   start; real window-hit accuracy is judged in calibration by the landing slot.",
        "runs": runs,
        "hours": rows,
    }))
}

#[cfg(test)]
mod tests {
    /// RFC 3339 with `Z`, never an offset (analysis/tests/test_utc.py checks the data side).
    fn assert_utc_z(s: &str) {
        assert!(s.ends_with('Z'), "{s}");
        let t = chrono::DateTime::parse_from_rfc3339(s).unwrap();
        assert_eq!(t.offset().local_minus_utc(), 0, "{s}");
    }

    #[test]
    fn hour_buckets_are_utc_z() {
        // 2026-10-25 00:30 UTC: the night Europe/Kyiv moves from UTC+3 to UTC+2
        let h = hour_of(1_792_888_200i128 * 1_000_000_000);
        assert_eq!(h, "2026-10-25T00:00:00Z");
        assert_utc_z(&h);
    }

    use super::*;

    const S: i128 = 1_000_000_000;

    #[test]
    fn detects_host_stall_and_resets_clock_after_long_ones() {
        let mut w = Watch::default();
        let t0 = 1_800_000_000 * S;
        assert!(w.tick(t0, Some(t0)).is_empty());
        assert!(w.tick(t0 + S, Some(t0 + S)).is_empty());
        let ev = w.tick(t0 + 9 * S, Some(t0 + 9 * S));
        assert_eq!(
            ev,
            vec![Event::Gap(Gap::new("host_stall", t0 + S, t0 + 9 * S, None))]
        );
        let ev = w.tick(t0 + 129 * S, Some(t0 + 129 * S));
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[1], Event::ResetClock);
    }

    #[test]
    fn feed_stall_is_logged_once_when_it_ends() {
        let mut w = Watch::default();
        let t0 = 1_800_000_000 * S;
        let last = t0;
        for k in 1..=10 {
            assert!(w.tick(t0 + k * S, Some(last)).is_empty(), "tick {k}");
        }
        let ev = w.tick(t0 + 11 * S, Some(t0 + 11 * S));
        assert_eq!(
            ev,
            vec![Event::Gap(Gap::new("slot_feed", t0, t0 + 11 * S, None))]
        );
        assert!(w.tick(t0 + 12 * S, Some(t0 + 12 * S)).is_empty());
        assert!(w.close(t0 + 13 * S).is_none());
    }

    #[test]
    fn hourly_buckets_errors_windows_and_split_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_path_buf();
        let h0 = 1_800_000_000 / 3600 * 3600 * S; // an hour boundary
        let clock: Vec<Value> = [
            (10, Some(50.0)),
            (20, Some(-150.0)),
            (3_700, Some(5.0)),
            (30, None),
        ]
        .iter()
        .map(|(sec, e)| json!({"predicted_prep_unix_ns": (h0 + sec * S) as i64, "err_prep_ms": e}))
        .collect();
        collect::append_jsonl(&d.join("clock.jsonl"), &clock).unwrap();
        let wins = vec![
            json!({"window": {"send_at_unix_ns": (h0 + 10 * S) as i64}, "outcome": "dry_run"}),
            json!({"window": {"send_at_unix_ns": (h0 + 40 * S) as i64}, "outcome": "skipped"}),
        ];
        collect::append_jsonl(&d.join("windows.jsonl"), &wins).unwrap();
        let g = Gap::new("host_stall", h0 + 3_590 * S, h0 + 3_620 * S, None);
        collect::append_jsonl(&d.join("gaps.jsonl"), &[g]).unwrap();
        let r = hourly(&[d]).unwrap();
        let hours = r["hours"].as_array().unwrap();
        assert_eq!(hours.len(), 2);
        assert_eq!(hours[0]["observed"], 2);
        assert_eq!(hours[0]["unobserved"], 1);
        assert_eq!(hours[0]["windows_executed"], 1);
        assert_eq!(hours[0]["windows_skipped"], 1);
        assert_eq!(hours[0]["err"]["max_abs_ms"], 150.0);
        assert_eq!(hours[0]["gap_secs"]["host_stall"], 10.0);
        assert_eq!(hours[1]["gap_secs"]["host_stall"], 20.0);
        assert_eq!(hours[1]["observed"], 1);
        assert_eq!(r["label"], "EXPLORATORY");
    }
}
