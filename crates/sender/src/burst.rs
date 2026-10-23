//! Burst executor: writes pre-signed transactions at planned offsets and records precise
//! per-transaction timing. All signing happens before; the loop only waits and writes.

use crate::live::LiveAuth;
use crate::quic::TpuConnection;
use crate::rpc::Rpc;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct BurstTx {
    pub idx: usize,
    pub sig: String,
    pub wire: Vec<u8>,
    pub offset_us: u64,
    /// Planned send position (arms.rs `send_rank`). Txs with equal offsets (every arm with a
    /// zero gap) go out in this order, never in idx order.
    pub rank: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendRecord {
    pub idx: usize,
    pub sig: String,
    pub path: String,
    pub planned_offset_us: u64,
    /// Planned send position; the actual position is the order of `start_mono_ns`.
    #[serde(default)]
    pub send_rank: usize,
    /// Monotonic ns relative to the burst's planned start.
    pub start_mono_ns: i64,
    pub end_mono_ns: i64,
    pub start_unix_ns: i128,
    pub end_unix_ns: i128,
    pub stream_id: Option<u64>,
    pub conn_id: Option<usize>,
    pub rtt_us: Option<u64>,
    pub error: Option<String>,
}

// Only used through generics inside this crate, so the missing Send bound is harmless.
#[allow(async_fn_in_trait)]
pub trait Sink {
    fn path(&self) -> &'static str;
    /// Pre-open whatever is needed for `n` sends (no bytes leave the process).
    async fn prepare(&mut self, n: usize) -> Result<()>;
    /// Write one transaction; returns the stream id if the path has one.
    async fn send(&mut self, slot: usize, bytes: &[u8]) -> Result<Option<u64>>;
    fn conn_id(&self) -> Option<usize> {
        None
    }
    fn rtt_us(&self) -> Option<u64> {
        None
    }
}

/// Dry-run sink: holds no connection and performs no I/O. It only counts.
#[derive(Default)]
pub struct DryRunSink {
    pub would_send: usize,
}

impl Sink for DryRunSink {
    fn path(&self) -> &'static str {
        "dry_run"
    }
    async fn prepare(&mut self, _n: usize) -> Result<()> {
        Ok(())
    }
    async fn send(&mut self, _slot: usize, _bytes: &[u8]) -> Result<Option<u64>> {
        self.would_send += 1;
        Ok(None)
    }
}

pub struct QuicSink<'a> {
    conn: &'a TpuConnection,
    auth: &'a LiveAuth,
    streams: Vec<Option<quinn::SendStream>>,
}

impl<'a> QuicSink<'a> {
    pub fn new(conn: &'a TpuConnection, auth: &'a LiveAuth) -> Self {
        Self {
            conn,
            auth,
            streams: vec![],
        }
    }
}

impl Sink for QuicSink<'_> {
    fn path(&self) -> &'static str {
        "quic"
    }
    async fn prepare(&mut self, n: usize) -> Result<()> {
        self.streams.clear();
        for _ in 0..n {
            self.streams
                .push(Some(self.conn.open_stream(self.auth).await?));
        }
        Ok(())
    }
    async fn send(&mut self, slot: usize, bytes: &[u8]) -> Result<Option<u64>> {
        let st = self.streams[slot]
            .take()
            .ok_or_else(|| anyhow::anyhow!("stream {slot} already used"))?;
        let w = TpuConnection::write_tx(self.auth, st, bytes).await?;
        Ok(Some(w.stream_id))
    }
    fn conn_id(&self) -> Option<usize> {
        Some(self.conn.info.stable_id)
    }
    fn rtt_us(&self) -> Option<u64> {
        Some(self.conn.rtt_us())
    }
}

/// Comparison path: the same burst through a standard RPC `sendTransaction`. `label` names
/// the path in the manifest (`rpc-helius-staked` for Helius, see `runner::rpc_path_label`).
/// The client's sendTransaction bucket (1/s on the Helius free plan) spaces the sends, so
/// the actual send times, not the planned offsets, describe this path.
pub struct RpcSink<'a> {
    rpc: &'a Rpc,
    auth: &'a LiveAuth,
    label: &'static str,
}

impl<'a> RpcSink<'a> {
    pub fn new(rpc: &'a Rpc, auth: &'a LiveAuth, label: &'static str) -> Self {
        Self { rpc, auth, label }
    }
}

impl Sink for RpcSink<'_> {
    fn path(&self) -> &'static str {
        self.label
    }
    async fn prepare(&mut self, _n: usize) -> Result<()> {
        Ok(())
    }
    async fn send(&mut self, _slot: usize, bytes: &[u8]) -> Result<Option<u64>> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        self.rpc.send_transaction(self.auth, &b64).await?;
        Ok(None)
    }
}

pub fn unix_ns() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

/// Spin margin: timer sleeps can overshoot by a full OS tick (~15.6 ms on Windows), so we
/// sleep until this far before `target` and spin the rest.
const SPIN_MARGIN: Duration = Duration::from_millis(20);

/// Sleep coarsely, then spin for the last `SPIN_MARGIN` to hit `target` precisely.
pub async fn wait_until(target: Instant) {
    let coarse = target.checked_sub(SPIN_MARGIN);
    if let Some(c) = coarse
        && c > Instant::now()
    {
        tokio::time::sleep_until(c.into()).await;
    }
    while Instant::now() < target {
        std::hint::spin_loop();
    }
}

/// Send `txs` (sorted by offset) starting at `start`. Failures are recorded, not fatal:
/// a failed write must still be accounted for by the ledger.
pub async fn run_burst<S: Sink>(sink: &mut S, txs: &[BurstTx], start: Instant) -> Vec<SendRecord> {
    let mut order: Vec<&BurstTx> = txs.iter().collect();
    // Offset first (gapped arms), then the randomized send rank. Sorting by offset alone kept
    // idx order whenever all offsets were 0, so arms P, R and C went out in price / CU-limit
    // order instead of their randomized order (LOGBOOK 2026-10-23 13:40).
    order.sort_by_key(|t| (t.offset_us, t.rank));
    let mut out = Vec::with_capacity(txs.len());
    let conn_id = sink.conn_id();
    for (slot, tx) in order.into_iter().enumerate() {
        let target = start + Duration::from_micros(tx.offset_us);
        wait_until(target).await;
        let t0 = Instant::now();
        let u0 = unix_ns();
        let res = sink.send(slot, &tx.wire).await;
        let t1 = Instant::now();
        let u1 = unix_ns();
        let rel = |t: Instant| -> i64 {
            if t >= start {
                (t - start).as_nanos() as i64
            } else {
                -((start - t).as_nanos() as i64)
            }
        };
        let (stream_id, error) = match res {
            Ok(s) => (s, None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        out.push(SendRecord {
            idx: tx.idx,
            sig: tx.sig.clone(),
            path: sink.path().to_string(),
            planned_offset_us: tx.offset_us,
            send_rank: tx.rank,
            start_mono_ns: rel(t0),
            end_mono_ns: rel(t1),
            start_unix_ns: u0,
            end_unix_ns: u1,
            stream_id,
            conn_id,
            rtt_us: sink.rtt_us(),
            error,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quic::{TpuClient, test_server};

    fn txs(gap_us: u64) -> Vec<BurstTx> {
        (0..4)
            .map(|i| BurstTx {
                idx: 3 - i,
                sig: format!("sig{i}"),
                wire: vec![i as u8; 200],
                offset_us: i as u64 * gap_us,
                rank: i,
            })
            .collect()
    }

    /// Zero gap, randomized ranks (arms P, R, C): the dry-run output must be in send_rank
    /// order, not idx order. Records come out in send order, and start_mono_ns increases.
    #[tokio::test]
    async fn zero_gap_burst_goes_out_in_send_rank_order() {
        for ranks in [
            [3usize, 4, 5, 1, 0, 2],
            [2, 1, 3, 0, 5, 4],
            [0, 1, 2, 3, 4, 5],
        ] {
            let burst: Vec<BurstTx> = ranks
                .iter()
                .enumerate()
                .map(|(idx, &rank)| BurstTx {
                    idx,
                    sig: format!("sig{idx}"),
                    wire: vec![idx as u8; 200],
                    offset_us: 0,
                    rank,
                })
                .collect();
            let mut sink = DryRunSink::default();
            sink.prepare(burst.len()).await.unwrap();
            let recs = run_burst(&mut sink, &burst, Instant::now()).await;
            let mut by_time: Vec<&SendRecord> = recs.iter().collect();
            by_time.sort_by_key(|r| r.start_mono_ns);
            let sent_ranks: Vec<usize> = by_time.iter().map(|r| ranks[r.idx]).collect();
            assert_eq!(sent_ranks, vec![0, 1, 2, 3, 4, 5], "ranks {ranks:?}");
            assert!(
                recs.windows(2)
                    .all(|w| w[0].start_mono_ns < w[1].start_mono_ns)
            );
            assert!(recs.iter().all(|r| r.send_rank == ranks[r.idx]));
        }
    }

    #[tokio::test]
    async fn dry_run_burst_never_touches_the_network() {
        let (_addr, seen) = test_server::spawn().await.unwrap();
        let mut sink = DryRunSink::default();
        sink.prepare(4).await.unwrap();
        let recs = run_burst(&mut sink, &txs(1_000), Instant::now()).await;
        assert_eq!(recs.len(), 4);
        assert_eq!(sink.would_send, 4);
        assert!(
            recs.iter()
                .all(|r| r.path == "dry_run" && r.stream_id.is_none())
        );
        let s = seen.lock().unwrap();
        assert_eq!((s.connections, s.streams.len()), (0, 0));
    }

    #[tokio::test]
    async fn live_burst_respects_gaps_and_send_order() {
        // The OS can preempt the spinning thread when other tests load the CPU (Windows,
        // LOGBOOK 2026-10-23). Production records the actual send times, so a late send is
        // measured, not hidden; here we require one of three attempts to be on time.
        let mut worst = vec![];
        for _attempt in 0..3 {
            let (addr, seen) = test_server::spawn().await.unwrap();
            let client = TpuClient::new().unwrap();
            let conn = client.connect(addr).await.unwrap();
            let auth = LiveAuth::for_tests(1);
            let mut sink = QuicSink::new(&conn, &auth);
            sink.prepare(4).await.unwrap();
            let start = Instant::now() + Duration::from_millis(20);
            let recs = run_burst(&mut sink, &txs(10_000), start).await;
            for (i, r) in recs.iter().enumerate() {
                assert!(r.error.is_none(), "{:?}", r.error);
                assert_eq!(r.sig, format!("sig{i}"));
            }
            let dev = recs
                .iter()
                .enumerate()
                .map(|(i, r)| (r.start_mono_ns - i as i64 * 10_000_000).abs())
                .max()
                .unwrap_or(0);
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(seen.lock().unwrap().streams.len(), 4);
            if dev < 2_000_000 {
                return;
            }
            worst.push(dev);
        }
        panic!("no attempt within 2 ms of plan; worst deviations (ns): {worst:?}");
    }
}
