//! Burst executor: writes pre-signed transactions at planned offsets and records precise
//! per-transaction timing. All signing happens before; the loop only waits and writes.

use crate::live::LiveAuth;
use crate::quic::TpuConnection;
use crate::rpc::Rpc;
use anyhow::Result;
use serde::Serialize;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct BurstTx {
    pub idx: usize,
    pub sig: String,
    pub wire: Vec<u8>,
    pub offset_us: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendRecord {
    pub idx: usize,
    pub sig: String,
    pub path: &'static str,
    pub planned_offset_us: u64,
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

/// Comparison path: the same burst through a standard RPC `sendTransaction`.
pub struct RpcSink<'a> {
    rpc: &'a Rpc,
    auth: &'a LiveAuth,
}

impl<'a> RpcSink<'a> {
    pub fn new(rpc: &'a Rpc, auth: &'a LiveAuth) -> Self {
        Self { rpc, auth }
    }
}

impl Sink for RpcSink<'_> {
    fn path(&self) -> &'static str {
        "rpc"
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
    order.sort_by_key(|t| t.offset_us);
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
            path: sink.path(),
            planned_offset_us: tx.offset_us,
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
            })
            .collect()
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
            let planned = i as i64 * 10_000_000;
            // within 2 ms of plan on a loaded CI box
            assert!(
                (r.start_mono_ns - planned).abs() < 2_000_000,
                "{i}: {}",
                r.start_mono_ns
            );
            assert_eq!(r.sig, format!("sig{i}"));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(seen.lock().unwrap().streams.len(), 4);
    }
}
