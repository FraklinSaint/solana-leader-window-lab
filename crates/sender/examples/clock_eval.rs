//! EXPLORATORY: replay recorded slot notifications and measure how well the slot clock
//! predicts the start of leader windows, for a grid of clock parameters.
//!
//! `cargo run --release --example clock_eval -- <slots.jsonl> [out.json]`
//!
//! Input: one `{"slot": u64, "t_ns": i128}` per line (a run's `slots.jsonl` or the output of
//! `slotfeed_probe`). For every observed window start (slot % 4 == 0; epochs start on
//! multiples of 432,000, so leader windows are 4-aligned) and every horizon H, the clock is
//! fitted on the notifications seen up to H before that slot was observed, and the error is
//! observed - predicted. This is the same quantity as `err_prep_ms` in a run's clock.jsonl
//! (H = prep time), computed offline so parameters can be compared on one recording.

use slwlab_sender::clock::{ClockParams, SlotClock};

fn quantile(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("usage: clock_eval <slots.jsonl> [out.json]"))?;
    let mut obs: Vec<(u64, i128)> = std::fs::read_to_string(path)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l)?;
            let slot = v["slot"].as_u64().ok_or_else(|| anyhow::anyhow!("slot"))?;
            let t = v["t_ns"]
                .as_i64()
                .map(i128::from)
                .or_else(|| v["t_ns"].as_f64().map(|f| f as i128))
                .ok_or_else(|| anyhow::anyhow!("t_ns"))?;
            Ok((slot, t))
        })
        .collect::<anyhow::Result<_>>()?;
    obs.sort();
    obs.dedup_by_key(|o| o.0);
    anyhow::ensure!(
        obs.len() > 400,
        "need > 400 notifications, got {}",
        obs.len()
    );
    let span_s = (obs[obs.len() - 1].1 - obs[0].1) as f64 / 1e9;
    println!(
        "EXPLORATORY clock replay: {} notifications, {:.0} s, slots {}..{}",
        obs.len(),
        span_s,
        obs[0].0,
        obs[obs.len() - 1].0
    );

    let windows = [100usize, 200, 300, 600];
    let anchors = [1usize, 3, 5, 9, 15];
    let horizons_ms = [500i128, 1500, 3000];
    let mut rows = vec![];
    println!(
        "{:>7} {:>6} {:>6} {:>6} {:>9} {:>9} {:>9} {:>9}",
        "window", "anchor", "H_ms", "n", "med|e|", "p90|e|", "p95|e|", "mean e"
    );
    for &h in &horizons_ms {
        for &window in &windows {
            for &anchor_k in &anchors {
                let p = ClockParams { window, anchor_k };
                let mut errs = vec![];
                for &(s, t_obs) in obs.iter().filter(|o| o.0 % 4 == 0) {
                    let cut = obs.partition_point(|o| o.1 <= t_obs - h * 1_000_000);
                    if cut < window.min(100) {
                        continue;
                    }
                    let mut c = SlotClock::with_params(p);
                    for &(k, t) in &obs[cut.saturating_sub(window)..cut] {
                        c.observe(k, t);
                    }
                    if let Some(f) = c.fit() {
                        errs.push((t_obs as f64 - f.predict_ns(s)) / 1e6);
                    }
                }
                if errs.is_empty() {
                    continue;
                }
                let mean = errs.iter().sum::<f64>() / errs.len() as f64;
                let mut abs: Vec<f64> = errs.iter().map(|e| e.abs()).collect();
                abs.sort_by(|a, b| a.total_cmp(b));
                let (med, p90, p95) = (
                    quantile(&abs, 0.5),
                    quantile(&abs, 0.9),
                    quantile(&abs, 0.95),
                );
                println!(
                    "{window:>7} {anchor_k:>6} {h:>6} {:>6} {med:>9.1} {p90:>9.1} {p95:>9.1} {mean:>9.1}",
                    errs.len()
                );
                rows.push(serde_json::json!({
                    "window": window, "anchor_k": anchor_k, "horizon_ms": h as i64,
                    "n": errs.len(), "median_abs_ms": med, "p90_abs_ms": p90,
                    "p95_abs_ms": p95, "mean_ms": mean,
                }));
            }
        }
    }
    if let Some(out) = args.get(2) {
        let doc = serde_json::json!({
            "label": "EXPLORATORY",
            "source": path,
            "notifications": obs.len(),
            "span_s": span_s,
            "rows": rows,
        });
        std::fs::write(out, serde_json::to_vec_pretty(&doc)?)?;
        println!("wrote {out}");
    }
    Ok(())
}
