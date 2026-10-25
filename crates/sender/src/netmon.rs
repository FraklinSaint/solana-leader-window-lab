//! Network path record of a session: the round-trip time to our RPC endpoint and the state of
//! the Wi-Fi link, sampled all session long by the supervisor.
//!
//! Why: this PC has no Ethernet, so every packet we send crosses a Wi-Fi link whose delay
//! varies. That is a property of the treatment path (README "Limitations: Wi-Fi"). The
//! round-trip time to the RPC provider is a *proxy* for it, not the delay of our QUIC sends to
//! a leader: it is an HTTP request on a kept-alive connection, so it includes the provider's
//! processing and the internet path as well as the Wi-Fi hop. It is reported as a noise level
//! (`analysis/slwlab/calibrate.py`), never used to correct a measurement.
//!
//! Files in `data/sessions/<id>/`: `rtt.jsonl` (one probe every [`RTT_EVERY`]), `wifi.jsonl`
//! (one link sample every [`WIFI_EVERY`]) and, at the end of the session, `network.json`
//! (summary, also in `coverage.json`). The first probe of a connection is marked `warmup`
//! and left out of the statistics (TLS handshake). The SSID is never read, the access point
//! is recorded only as an 8-hex-char hash of its BSSID, and the endpoint is never written.
//! A probe is one ~300 byte POST every 5 s, and counts as one `getSlot` credit.

use crate::rpc::Rpc;
use crate::session::{self, Stop};
use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub const RTT_EVERY: Duration = Duration::from_secs(5);
pub const WIFI_EVERY: Duration = Duration::from_secs(30);
/// Two probes further apart than this are not "consecutive" for the jitter statistic.
pub const MAX_PAIR_GAP_SECS: i64 = 15;

fn append(path: &Path, v: &Value) {
    let r = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "{v}"));
    if let Err(e) = r {
        tracing::warn!(path = %path.display(), error = %e, "network record not written");
    }
}

/// JSON of one Wi-Fi link reading (no SSID; the BSSID only as a short hash).
pub fn wifi_json(l: &slwlab_winsys::WifiLink) -> Value {
    let ap = hex::encode(Sha256::digest(l.bssid))[..8].to_string();
    json!({
        "channel": l.channel,
        "band": l.channel.map(slwlab_winsys::band_of_channel),
        "signal_quality": l.signal_quality,
        "rssi_dbm": l.rssi_dbm,
        "phy": l.phy,
        "rx_mbps": l.rx_mbps,
        "tx_mbps": l.tx_mbps,
        "ap_id": ap,
    })
}

/// One `wifi.jsonl` line: the default-route network type and, on Wi-Fi, the link reading.
pub fn wifi_sample(unix: i64) -> Value {
    let adapter = slwlab_winsys::default_route_adapter();
    let net = crate::host::classify_network(adapter.as_ref());
    let link = (net == "wifi")
        .then(slwlab_winsys::wifi_link)
        .flatten()
        .map(|l| wifi_json(&l));
    json!({"t": session::utc(unix), "unix": unix, "network_type": net, "wifi": link})
}

/// Sample the path until `stop` is set (checked every probe).
pub async fn monitor(dir: std::path::PathBuf, rpc: Rpc, stop: Arc<Stop>) {
    let rtt_path = dir.join("rtt.jsonl");
    let wifi_path = dir.join("wifi.jsonl");
    let wifi_every = (WIFI_EVERY.as_secs() / RTT_EVERY.as_secs()).max(1);
    let mut tick = 0u64;
    let mut warm = false;
    while !stop.is_set() {
        let now = chrono::Utc::now().timestamp();
        if tick.is_multiple_of(wifi_every) {
            append(&wifi_path, &wifi_sample(now));
        }
        let line = match rpc.probe_rtt().await {
            Ok(d) => {
                let mut v = json!({"t": session::utc(now), "unix": now,
                                   "rtt_ms": d.as_secs_f64() * 1000.0});
                if !warm {
                    v["warmup"] = json!(true);
                    warm = true;
                }
                v
            }
            Err(e) => {
                warm = false; // the next success may open a new connection
                json!({"t": session::utc(now), "unix": now, "error": format!("{e:#}")})
            }
        };
        append(&rtt_path, &line);
        tick += 1;
        tokio::time::sleep(RTT_EVERY).await;
    }
}

/// Nearest-rank percentile of a sorted slice (`q` in 0..=1).
pub fn percentile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (q * sorted.len() as f64).ceil().max(1.0) as usize;
    Some(sorted[rank.min(sorted.len()) - 1])
}

fn median(sorted: &[f64]) -> Option<f64> {
    percentile(sorted, 0.5)
}

/// Distribution of round-trip times (ms) of consecutive probes `(unix_s, rtt_ms)`; `None` in
/// the second slot is a failed probe.
///
/// - `sigma_mad_ms`: 1.4826 x median absolute deviation (robust spread);
/// - `jitter_ms`: mean absolute difference of consecutive successful probes (at most
///   [`MAX_PAIR_GAP_SECS`] apart), the RFC 3550 interarrival jitter without smoothing.
pub fn rtt_stats(samples: &[(i64, Option<f64>)]) -> Value {
    let ok: Vec<f64> = samples.iter().filter_map(|s| s.1).collect();
    let mut sorted = ok.clone();
    sorted.sort_by(f64::total_cmp);
    let n_err = samples.len() - ok.len();
    let mut diffs = vec![];
    for w in samples.windows(2) {
        if let ((t0, Some(a)), (t1, Some(b))) = (w[0], w[1])
            && t1 - t0 <= MAX_PAIR_GAP_SECS
        {
            diffs.push((b - a).abs());
        }
    }
    let med = median(&sorted);
    let mad = med.and_then(|m| {
        let mut d: Vec<f64> = sorted.iter().map(|x| (x - m).abs()).collect();
        d.sort_by(f64::total_cmp);
        median(&d)
    });
    json!({
        "n": samples.len(), "n_ok": ok.len(), "n_err": n_err,
        "loss_share": (!samples.is_empty()).then(|| n_err as f64 / samples.len() as f64),
        "min_ms": sorted.first(), "p50_ms": med,
        "p90_ms": percentile(&sorted, 0.90), "p95_ms": percentile(&sorted, 0.95),
        "p99_ms": percentile(&sorted, 0.99), "max_ms": sorted.last(),
        "sigma_mad_ms": mad.map(|m| 1.4826 * m),
        "jitter_ms": (!diffs.is_empty()).then(|| diffs.iter().sum::<f64>() / diffs.len() as f64),
        "n_pairs": diffs.len(),
    })
}

fn read_lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn count_by(vals: impl Iterator<Item = String>) -> Value {
    let mut m: BTreeMap<String, u64> = BTreeMap::new();
    for v in vals {
        *m.entry(v).or_default() += 1;
    }
    json!(m)
}

/// Summary of a session's `rtt.jsonl` and `wifi.jsonl`. `preflight` is the pre-flight's
/// network block (kept so one file tells the whole story).
pub fn summarize(dir: &Path, preflight: Value) -> Value {
    let rtt: Vec<(i64, Option<f64>, bool)> = read_lines(&dir.join("rtt.jsonl"))
        .iter()
        .filter_map(|v| {
            Some((
                v["unix"].as_i64()?,
                v["rtt_ms"].as_f64(),
                v["warmup"].as_bool().unwrap_or(false),
            ))
        })
        .collect();
    let used: Vec<(i64, Option<f64>)> = rtt.iter().filter(|s| !s.2).map(|s| (s.0, s.1)).collect();
    let mut by_hour: BTreeMap<String, Vec<(i64, Option<f64>)>> = BTreeMap::new();
    for s in &used {
        by_hour
            .entry(crate::gaps::hour_of(i128::from(s.0) * 1_000_000_000))
            .or_default()
            .push(*s);
    }
    let hours: Vec<Value> = by_hour
        .iter()
        .map(|(h, v)| {
            let mut st = rtt_stats(v);
            st["hour_utc"] = json!(h);
            st
        })
        .collect();

    let wifi = read_lines(&dir.join("wifi.jsonl"));
    let links: Vec<&Value> = wifi.iter().filter(|w| !w["wifi"].is_null()).collect();
    let nums = |k: &str| -> Vec<f64> {
        let mut v: Vec<f64> = links.iter().filter_map(|w| w["wifi"][k].as_f64()).collect();
        v.sort_by(f64::total_cmp);
        v
    };
    let spread = |v: &[f64]| {
        json!({"min": v.first(), "p5": percentile(v, 0.05), "median": median(v),
               "p95": percentile(v, 0.95), "max": v.last()})
    };
    let aps: Vec<&str> = links
        .iter()
        .filter_map(|w| w["wifi"]["ap_id"].as_str())
        .collect();
    let ap_changes = aps.windows(2).filter(|w| w[0] != w[1]).count();
    json!({
        "label": "PATH RECORD (Wi-Fi limitation; README, PREREGISTRATION)",
        "method": "rtt: JSON-RPC getSlot over a kept-alive HTTPS connection, one probe per 5 s, \
                   first probe of a connection excluded; a proxy for the path, not the delay of \
                   QUIC sends to a leader. wifi: WLAN API sample per 30 s (SSID never read).",
        "preflight": preflight,
        "rtt": rtt_stats(&used),
        "rtt_warmup_probes": rtt.iter().filter(|s| s.2).count(),
        "rtt_by_hour": hours,
        "wifi_samples": wifi.len(),
        "wifi_link_samples": links.len(),
        "network_types": count_by(wifi.iter().filter_map(|w| w["network_type"].as_str().map(str::to_string))),
        "bands": count_by(links.iter().filter_map(|w| w["wifi"]["band"].as_str().map(str::to_string))),
        "channels": count_by(links.iter().filter_map(|w| w["wifi"]["channel"].as_u64().map(|c| c.to_string()))),
        "phys": count_by(links.iter().filter_map(|w| w["wifi"]["phy"].as_str().map(str::to_string))),
        "access_points": aps.iter().collect::<std::collections::BTreeSet<_>>().len(),
        "access_point_changes": ap_changes,
        "signal_quality": spread(&nums("signal_quality")),
        "rssi_dbm": spread(&nums("rssi_dbm")),
        "rx_mbps": spread(&nums("rx_mbps")),
        "tx_mbps": spread(&nums("tx_mbps")),
    })
}

/// Write `network.json` next to the session's other files and return the summary.
pub fn write_summary(dir: &Path, preflight: Value) -> Result<Value> {
    let v = summarize(dir, preflight);
    std::fs::write(dir.join("network.json"), serde_json::to_vec_pretty(&v)?)?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 0.5), Some(50.0));
        assert_eq!(percentile(&v, 0.95), Some(95.0));
        assert_eq!(percentile(&v, 0.99), Some(99.0));
        assert_eq!(percentile(&v, 1.0), Some(100.0));
        assert_eq!(percentile(&[7.0], 0.95), Some(7.0));
        assert_eq!(percentile(&[], 0.5), None);
    }

    #[test]
    fn rtt_stats_known_values() {
        // 10, 20, 10, 20 at 5 s spacing: median 15 (nearest rank: 10), jitter 10
        let s: Vec<(i64, Option<f64>)> = [10.0, 20.0, 10.0, 20.0]
            .iter()
            .enumerate()
            .map(|(i, r)| (i as i64 * 5, Some(*r)))
            .collect();
        let v = rtt_stats(&s);
        assert_eq!(v["n"], 4);
        assert_eq!(v["n_err"], 0);
        assert_eq!(v["jitter_ms"], 10.0);
        assert_eq!(v["min_ms"], 10.0);
        assert_eq!(v["max_ms"], 20.0);
        assert_eq!(v["n_pairs"], 3);
    }

    #[test]
    fn failures_and_gaps_do_not_make_pairs() {
        let s = vec![
            (0, Some(10.0)),
            (5, None),
            (10, Some(30.0)),
            (100, Some(50.0)), // 90 s after the previous: not consecutive
            (105, Some(50.0)),
        ];
        let v = rtt_stats(&s);
        assert_eq!(v["n_err"], 1);
        assert_eq!(v["loss_share"], 0.2);
        assert_eq!(v["n_pairs"], 1);
        assert_eq!(v["jitter_ms"], 0.0);
        assert!(rtt_stats(&[])["p50_ms"].is_null());
    }

    #[test]
    fn mad_sigma_of_a_spread_sample() {
        let s: Vec<(i64, Option<f64>)> = (0..9).map(|i| (i * 5, Some(10.0 + i as f64))).collect();
        // median 14, absolute deviations 0..4 twice -> median 2 -> sigma 2.9652
        let v = rtt_stats(&s);
        assert!((v["sigma_mad_ms"].as_f64().unwrap() - 2.9652).abs() < 1e-9);
    }

    #[test]
    fn summary_reads_the_session_files_and_excludes_warmup() {
        let d = tempfile::tempdir().unwrap();
        let rtt = [
            json!({"unix": 1_000, "rtt_ms": 300.0, "warmup": true}),
            json!({"unix": 1_005, "rtt_ms": 20.0}),
            json!({"unix": 1_010, "rtt_ms": 30.0}),
            json!({"unix": 1_015, "error": "timeout"}),
        ];
        let wifi = [
            json!({"unix": 1_000, "network_type": "wifi", "wifi": {"channel": 36, "band": "5 GHz",
                   "signal_quality": 80, "rssi_dbm": -60, "phy": "802.11ac", "rx_mbps": 200,
                   "tx_mbps": 150, "ap_id": "aaaaaaaa"}}),
            json!({"unix": 1_030, "network_type": "wifi", "wifi": {"channel": 36, "band": "5 GHz",
                   "signal_quality": 60, "rssi_dbm": -70, "phy": "802.11ac", "rx_mbps": 100,
                   "tx_mbps": 100, "ap_id": "bbbbbbbb"}}),
            json!({"unix": 1_060, "network_type": "wifi", "wifi": null}),
        ];
        let lines = |v: &[Value]| v.iter().map(|x| format!("{x}\n")).collect::<String>();
        std::fs::write(d.path().join("rtt.jsonl"), lines(&rtt)).unwrap();
        std::fs::write(d.path().join("wifi.jsonl"), lines(&wifi)).unwrap();
        let s = write_summary(d.path(), json!({"type": "wifi"})).unwrap();
        assert!(d.path().join("network.json").exists());
        assert_eq!(s["rtt"]["n"], 3);
        assert_eq!(s["rtt"]["n_ok"], 2);
        assert_eq!(s["rtt"]["max_ms"], 30.0, "warmup probe is excluded");
        assert_eq!(s["rtt_warmup_probes"], 1);
        assert_eq!(s["rtt"]["jitter_ms"], 10.0);
        assert_eq!(s["bands"]["5 GHz"], 2);
        assert_eq!(s["network_types"]["wifi"], 3);
        assert_eq!(s["access_points"], 2);
        assert_eq!(s["access_point_changes"], 1);
        assert_eq!(s["signal_quality"]["min"], 60.0);
        assert_eq!(s["preflight"]["type"], "wifi");
        // no files at all: an empty but well-formed summary
        let e = summarize(tempfile::tempdir().unwrap().path(), Value::Null);
        assert_eq!(e["rtt"]["n"], 0);
    }

    #[test]
    fn wifi_json_hides_the_bssid() {
        let l = slwlab_winsys::WifiLink {
            channel: Some(149),
            signal_quality: 77,
            rssi_dbm: Some(-61),
            phy: "802.11ac",
            rx_mbps: 300,
            tx_mbps: 200,
            bssid: [1, 2, 3, 4, 5, 6],
        };
        let j = wifi_json(&l);
        assert_eq!(j["band"], "5 GHz");
        assert_eq!(j["ap_id"].as_str().unwrap().len(), 8);
        assert!(!j.to_string().contains("01:02"));
    }
    /// Manual (`RPC_URL` in the environment; about 3 read-only getSlot credits):
    /// `cargo test -p slwlab-sender --lib netmon -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn monitor_against_the_real_endpoint() {
        let Ok(url) = std::env::var("RPC_URL") else {
            return;
        };
        let d = tempfile::tempdir().unwrap();
        let stop = Arc::new(Stop::default());
        let task = tokio::spawn(monitor(
            d.path().to_path_buf(),
            Rpc::new(&url),
            stop.clone(),
        ));
        tokio::time::sleep(Duration::from_secs(12)).await;
        stop.request("test");
        task.await.unwrap();
        let s = summarize(d.path(), Value::Null);
        println!("rtt: {}", s["rtt"]);
        println!("warmup probes: {}", s["rtt_warmup_probes"]);
        println!(
            "network types: {}  bands: {}",
            s["network_types"], s["bands"]
        );
        assert!(s["rtt"]["n_ok"].as_u64().unwrap() >= 1, "{s}");
        assert_eq!(s["rtt_warmup_probes"], 1);
    }
}
