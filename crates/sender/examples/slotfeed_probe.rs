//! Diagnostic: record slot notifications for N seconds and show how the clock fit behaves.
//! `cargo run --example slotfeed_probe -- <rpc_url> <seconds> [out.jsonl]`

use slwlab_sender::{clock::SlotClock, feed, rpc::Rpc};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let url = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "https://api.mainnet-beta.solana.com".into());
    let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(60);
    let rpc = Rpc::new(&url);
    let state = feed::new_state();
    feed::spawn(Some(feed::ws_from_http(&url)), rpc, state.clone());
    tokio::time::sleep(Duration::from_secs(secs)).await;
    let s = state.lock().map_err(|_| anyhow::anyhow!("poisoned"))?;
    let obs: Vec<(u64, i128)> = s.first_seen.iter().map(|(k, v)| (*k, *v)).collect();
    if let Some(out) = args.get(3) {
        let lines: Vec<String> = obs
            .iter()
            .map(|(k, v)| format!("{{\"slot\":{k},\"t_ns\":{v}}}"))
            .collect();
        std::fs::write(out, lines.join("\n"))?;
    }
    println!("source={} observations={}", s.source, obs.len());
    if let (Some(a), Some(b)) = (obs.first(), obs.last()) {
        println!(
            "span: {} slots in {:.1} s => {:.1} ms/slot (endpoints)",
            b.0 - a.0,
            (b.1 - a.1) as f64 / 1e9,
            (b.1 - a.1) as f64 / 1e6 / (b.0 - a.0) as f64
        );
    }
    // inter-arrival gaps between consecutive notifications
    let mut gaps: Vec<f64> = obs
        .windows(2)
        .map(|w| (w[1].1 - w[0].1) as f64 / 1e6)
        .collect();
    gaps.sort_by(|a, b| a.total_cmp(b));
    let q = |p: f64| gaps[((gaps.len() - 1) as f64 * p) as usize];
    if !gaps.is_empty() {
        println!(
            "notification gaps ms: p10 {:.1} p50 {:.1} p90 {:.1} max {:.1}",
            q(0.1),
            q(0.5),
            q(0.9),
            q(1.0)
        );
    }
    let skipped = obs.windows(2).filter(|w| w[1].0 != w[0].0 + 1).count();
    println!("slot jumps (missing notifications): {skipped}");
    for n in [30, 100, 300, 1000, obs.len()] {
        if n > obs.len() || n < 3 {
            continue;
        }
        let mut c = SlotClock::new(n);
        for &(k, t) in &obs[obs.len() - n..] {
            c.observe(k, t);
        }
        if let Some(f) = c.fit() {
            println!(
                "last {n:>5} obs: {:.2} ms/slot, resid {:.1} ms, used {} rejected {}",
                f.ns_per_slot / 1e6,
                f.resid_ms,
                f.n_used,
                f.n_rejected
            );
        }
    }
    Ok(())
}
