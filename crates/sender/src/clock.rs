//! Slot clock: maps slots to local wall-clock time from slot notifications.
//!
//! Observations are `(slot, t_ns)` = the local UNIX time at which we first learned that
//! `slot` exists (slotSubscribe notification, or getSlot polling as a fallback). The model is
//! an ordinary-least-squares line `t = a + b * slot` over a sliding window, refit after
//! rejecting residuals beyond 4 MADs (skipped/late notifications). It predicts when we will
//! *observe* a slot, which differs from when the leader starts building it by a constant
//! offset; the calibration offset sweep absorbs that constant.
//!
//! Slot timing wanders (residuals of a straight line over minutes are autocorrelated, lag-1
//! ~0.8 on mainnet, 2026-10-22), so the line is *re-anchored*: the slope comes from the whole
//! window, the intercept from the median of the last [`ANCHOR_K`] observations. On 90 s of
//! public-RPC notifications this cut the 1.5 s-ahead prediction error from 148 to 94 ms
//! (median) and from 531 to 356 ms (p90) versus the plain fit (LOGBOOK 2026-10-22).
//!
//! Both parameters ([`ClockParams`]) are CLI options and are recorded in every run.json,
//! because the best values depend on the feed: they were tuned on the public RPC and are
//! re-chosen for the Helius slotSubscribe feed with `examples/clock_eval.rs` (exploratory)
//! before the prereg freeze.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Observations used for the intercept (re-anchoring), see module docs.
pub const ANCHOR_K: usize = 5;
/// Default sliding window for the slope (~65 s of slots).
pub const WINDOW: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockParams {
    /// Observations in the sliding window (slope).
    pub window: usize,
    /// Most recent observations whose median residual sets the intercept.
    pub anchor_k: usize,
}

impl Default for ClockParams {
    fn default() -> Self {
        Self {
            window: WINDOW,
            anchor_k: ANCHOR_K,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Fit {
    /// Intercept at `slot0` in ns (we fit around `slot0` to keep the numbers small).
    pub t0_ns: f64,
    pub slot0: u64,
    pub ns_per_slot: f64,
    pub n_used: usize,
    pub n_rejected: usize,
    /// Robust residual scale (1.4826 * MAD) in ms.
    pub resid_ms: f64,
}

impl Fit {
    pub fn predict_ns(&self, slot: u64) -> f64 {
        self.t0_ns + (slot as f64 - self.slot0 as f64) * self.ns_per_slot
    }
}

pub struct SlotClock {
    window: usize,
    anchor_k: usize,
    obs: VecDeque<(u64, i128)>,
}

fn ols(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    let n = points.len() as f64;
    if points.len() < 3 {
        return None;
    }
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let sxx: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
    if sxx == 0.0 {
        return None;
    }
    let sxy: f64 = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let b = sxy / sxx;
    Some((my - b * mx, b))
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let m = v.len() / 2;
    if v.len().is_multiple_of(2) {
        (v[m - 1] + v[m]) / 2.0
    } else {
        v[m]
    }
}

impl SlotClock {
    pub fn new(window: usize) -> Self {
        Self::with_params(ClockParams {
            window,
            ..ClockParams::default()
        })
    }

    pub fn with_params(p: ClockParams) -> Self {
        let window = p.window.max(3);
        Self {
            window,
            anchor_k: p.anchor_k.max(1),
            obs: VecDeque::with_capacity(window + 1),
        }
    }

    /// Record the first time we saw `slot`. Duplicates and regressions are ignored.
    pub fn observe(&mut self, slot: u64, t_unix_ns: i128) {
        if self.obs.back().is_some_and(|&(s, _)| slot <= s) {
            return;
        }
        self.obs.push_back((slot, t_unix_ns));
        while self.obs.len() > self.window {
            self.obs.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.obs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.obs.is_empty()
    }

    pub fn fit(&self) -> Option<Fit> {
        let &(slot0, t_first) = self.obs.front()?;
        let pts: Vec<(f64, f64)> = self
            .obs
            .iter()
            .map(|&(s, t)| ((s - slot0) as f64, (t - t_first) as f64))
            .collect();
        let (a, b) = ols(&pts)?;
        let resid: Vec<f64> = pts.iter().map(|p| p.1 - (a + b * p.0)).collect();
        let mad = 1.4826 * median(resid.iter().map(|r| r.abs()).collect());
        let keep: Vec<(f64, f64)> = pts
            .iter()
            .zip(&resid)
            .filter(|(_, r)| mad == 0.0 || r.abs() <= 4.0 * mad)
            .map(|(p, _)| *p)
            .collect();
        let (_, b) = ols(&keep).unwrap_or((a, b));
        let tail = &pts[pts.len().saturating_sub(self.anchor_k)..];
        let a = median(tail.iter().map(|p| p.1 - b * p.0).collect());
        Some(Fit {
            t0_ns: t_first as f64 + a,
            slot0,
            ns_per_slot: b,
            n_used: keep.len(),
            n_rejected: pts.len() - keep.len(),
            resid_ms: mad / 1e6,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_slot_time_with_jitter_and_outliers() {
        let mut c = SlotClock::new(2000);
        let base: i128 = 1_792_600_000_000_000_000;
        let mut x: u64 = 12345;
        for i in 0..1500u64 {
            // deterministic pseudo-jitter in [-5ms, 5ms]
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let jitter = ((x >> 33) % 10_000_000) as i128 - 5_000_000;
            let late = if i % 97 == 0 { 400_000_000 } else { 0 }; // late notifications
            if i % 13 == 5 {
                continue; // skipped slots: no observation
            }
            c.observe(
                460_000_000 + i,
                base + i as i128 * 212_000_000 + jitter + late,
            );
        }
        let f = c.fit().unwrap();
        assert!((f.ns_per_slot - 212e6).abs() < 0.2e6, "{}", f.ns_per_slot);
        assert!(f.n_rejected >= 10);
        let pred = f.predict_ns(460_002_000);
        let truth = (base + 2000 * 212_000_000) as f64;
        assert!(
            (pred - truth).abs() < 5e6,
            "err ms {}",
            (pred - truth) / 1e6
        );
    }

    #[test]
    fn re_anchoring_tracks_wander() {
        // 300 slots at 212 ms, then the chain falls 300 ms behind over 20 slots and keeps
        // the new phase. A plain line would split the difference; the anchored fit follows.
        let mut c = SlotClock::new(300);
        let mut t: i128 = 0;
        for i in 0..340u64 {
            t += 212_000_000
                + if (300..320).contains(&i) {
                    15_000_000
                } else {
                    0
                };
            c.observe(i, t);
        }
        let f = c.fit().unwrap();
        let truth = (t + 7 * 212_000_000) as f64;
        assert!(
            (f.predict_ns(346) - truth).abs() < 20e6,
            "{}",
            (f.predict_ns(346) - truth) / 1e6
        );
    }

    #[test]
    fn ignores_duplicates_and_regressions() {
        let mut c = SlotClock::new(10);
        c.observe(10, 0);
        c.observe(10, 5);
        c.observe(9, 7);
        assert_eq!(c.len(), 1);
        assert!(c.fit().is_none());
    }
}
