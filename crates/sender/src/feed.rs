//! Slot feed for the clock model: `slotSubscribe` over WebSocket, with a getSlot polling
//! fallback (logged, since it degrades timing precision). Every observation is the local
//! UNIX time at which we first learned about a slot.
//!
//! Only the standard `slotSubscribe` is used. `slotsUpdatesSubscribe` is unstable and not
//! offered by every provider (Helius LaserStream WSS does not support it), and provider
//! extensions (transactionSubscribe, blockSubscribe) are excluded by design.
//! At most [`MAX_WS_CONNECTIONS`] WebSockets are open per process (Helius free plan: 5).
//! Every received frame is counted by the credit meter. The URL carries the API key, so it
//! is logged only redacted.

use crate::burst::unix_ns;
use crate::clock::{ClockParams, SlotClock};
use crate::redact::redact;
use crate::rpc::Rpc;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio_tungstenite::tungstenite::Message;

pub struct SlotState {
    pub clock: SlotClock,
    /// First-seen time per slot (bounded), for predicted-vs-observed reports.
    pub first_seen: BTreeMap<u64, i128>,
    pub source: &'static str,
    /// Local time of the newest WebSocket notification (polling does not count: its timing
    /// is too coarse for the clock model). Read by the runner's staleness gate.
    pub last_ws_ns: Option<i128>,
}

impl SlotState {
    /// `Some(reason)` if the WebSocket feed is stale at `now`: no notification yet, or none
    /// for more than [`STALE_AFTER_NS`]. A window must not be prepared or sent then, because
    /// the slot clock is extrapolating from old (or poll-only) observations.
    pub fn stale_reason(&self, now: i128) -> Option<String> {
        let limit_ms = STALE_AFTER_NS / 1_000_000;
        match self.last_ws_ns {
            None => Some("slot feed stale: no WebSocket notification yet".into()),
            Some(t) if now - t > STALE_AFTER_NS => Some(format!(
                "slot feed stale: last WebSocket notification {} ms ago (limit {limit_ms} ms)",
                (now - t) / 1_000_000
            )),
            Some(_) => None,
        }
    }

    /// Age of the newest WebSocket notification at `now`, in ms.
    pub fn ws_age_ms(&self, now: i128) -> Option<i64> {
        self.last_ws_ns.map(|t| ((now - t) / 1_000_000) as i64)
    }
}

pub type Shared = Arc<Mutex<SlotState>>;

/// Process-wide cap on open WebSocket connections (provider limit 5, one kept spare).
pub const MAX_WS_CONNECTIONS: usize = 4;
static WS_PERMITS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_WS_CONNECTIONS)));

/// WebSocket connections currently open in this process.
pub fn ws_open() -> usize {
    MAX_WS_CONNECTIONS - WS_PERMITS.available_permits()
}

/// The WebSocket feed is stale after this long without a notification.
///
/// Measured on every Helius slotSubscribe recording we have (2026-10-22/23, ~80 min,
/// 22,707 inter-arrival gaps: `data/exploratory/clock_eval/helius_slots_20261022T182628Z.jsonl`
/// and `slots.jsonl` of dryrun-20261022T183752Z, -20261023T130118Z, -20261023T210442Z): median
/// 210 ms, p99 397 ms, p99.9 544 ms, p99.99 775 ms, max 889 ms, none above 1 s. 2 s is 2.25x the
/// largest gap ever seen (no false alarm on that data) and ~5 slots, so a dead socket is caught
/// before the clock's extrapolation error can grow much past its own p95 (~300 ms).
pub const STALE_AFTER_NS: i128 = 2_000_000_000;

/// A silent socket is given up after the staleness limit, so a stall triggers a reconnect.
const WS_READ_TIMEOUT: Duration = Duration::from_nanos(STALE_AFTER_NS as u64);

/// At most one immediate reconnect per this interval; otherwise poll 5 s first.
const FAST_RECONNECT_EVERY: Duration = Duration::from_secs(30);

/// getSlot polling interval of the fallback: 2 req/s, a quarter of the RPC budget.
const POLL_EVERY: Duration = Duration::from_millis(500);

pub fn new_state() -> Shared {
    new_state_with(ClockParams::default())
}

pub fn new_state_with(p: ClockParams) -> Shared {
    Arc::new(Mutex::new(SlotState {
        clock: SlotClock::with_params(p),
        first_seen: BTreeMap::new(),
        source: "none",
        last_ws_ns: None,
    }))
}

pub(crate) fn record(state: &Shared, slot: u64, t: i128, source: &'static str) {
    let mut s = state.lock().expect("slot state");
    s.source = source;
    if source == "ws" {
        s.last_ws_ns = Some(t);
    }
    s.first_seen.entry(slot).or_insert(t);
    while s.first_seen.len() > 20_000 {
        s.first_seen.pop_first();
    }
    s.clock.observe(slot, t);
}

/// One WebSocket session; always ends with an error. `got` counts its slot notifications.
/// `permits` is the pool of open-connection slots: the process-wide [`WS_PERMITS`] in
/// production, a private pool in the cap test (a shared one would starve parallel tests).
async fn ws_loop(
    ws_url: &str,
    rpc: &Rpc,
    state: &Shared,
    got: &mut u64,
    permits: &Arc<Semaphore>,
) -> anyhow::Result<()> {
    let _permit = permits.clone().try_acquire_owned().map_err(|_| {
        anyhow::anyhow!("WebSocket cap reached ({MAX_WS_CONNECTIONS} open in this process)")
    })?;
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url)
        .await
        .map_err(|e| anyhow::anyhow!("connect: {}", redact(&e.to_string())))?;
    ws.send(Message::Text(
        json!({"jsonrpc":"2.0","id":1,"method":"slotSubscribe"})
            .to_string()
            .into(),
    ))
    .await?;
    tracing::info!(ws_url = %redact(ws_url), open = ws_open(), "slotSubscribe connected");
    // A socket can die silently (host sleep, NAT timeout): with ~5 notifications/s and no gap
    // above 889 ms ever measured, STALE_AFTER_NS of silence means it is gone. Bail out so the
    // caller reconnects.
    loop {
        let msg = match tokio::time::timeout(WS_READ_TIMEOUT, ws.next()).await {
            Err(_) => anyhow::bail!("no message for {} ms", WS_READ_TIMEOUT.as_millis()),
            Ok(None) => break,
            Ok(Some(m)) => m,
        };
        let t = unix_ns();
        let msg = msg?;
        rpc.meter().ws_bytes(msg.len());
        let Message::Text(text) = msg else { continue };
        let v: Value = serde_json::from_str(&text)?;
        if let Some(slot) = v["params"]["result"]["slot"].as_u64() {
            record(state, slot, t, "ws");
            *got += 1;
        }
    }
    anyhow::bail!("websocket closed")
}

async fn poll_loop(rpc: &Rpc, state: &Shared, for_dur: Duration) {
    let end = tokio::time::Instant::now() + for_dur;
    while tokio::time::Instant::now() < end {
        if let Ok(slot) = rpc.slot().await {
            record(state, slot, unix_ns(), "poll");
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// Runs forever: WebSocket when available, otherwise getSlot polling. A socket that had
/// delivered notifications and then failed (stall, close) is reconnected at once, at most once
/// per [`FAST_RECONNECT_EVERY`]; any other failure polls for 5 s before the next attempt
/// (public endpoints close the socket every few minutes).
pub fn spawn(ws_url: Option<String>, rpc: Rpc, state: Shared) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_fast: Option<tokio::time::Instant> = None;
        loop {
            if let Some(url) = &ws_url {
                let mut got = 0;
                if let Err(e) = ws_loop(url, &rpc, &state, &mut got, &WS_PERMITS).await {
                    let fast =
                        got > 0 && last_fast.is_none_or(|t| t.elapsed() >= FAST_RECONNECT_EVERY);
                    let error = redact(&format!("{e:#}")).into_owned();
                    if fast {
                        tracing::warn!(%error, notifications = got, "slot websocket failed; reconnecting now");
                        last_fast = Some(tokio::time::Instant::now());
                        continue;
                    }
                    tracing::warn!(%error, notifications = got, "slot websocket failed; polling for 5 s, then reconnecting");
                }
            }
            poll_loop(
                &rpc,
                &state,
                Duration::from_secs(if ws_url.is_some() { 5 } else { 60 }),
            )
            .await;
        }
    })
}

/// Derive a WebSocket URL from an HTTP RPC URL (http->ws, https->wss, port+1 for localhost).
pub fn ws_from_http(http: &str) -> String {
    let ws = http
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    if let Some(rest) = ws.strip_prefix("ws://127.0.0.1:")
        && let Ok(port) = rest.trim_end_matches('/').parse::<u16>()
    {
        return format!("ws://127.0.0.1:{}", port + 1);
    }
    ws
}

#[cfg(test)]
mod tests {
    #[test]
    fn ws_url_derivation() {
        assert_eq!(
            super::ws_from_http("https://api.mainnet-beta.solana.com"),
            "wss://api.mainnet-beta.solana.com"
        );
        assert_eq!(
            super::ws_from_http("http://127.0.0.1:8899"),
            "ws://127.0.0.1:8900"
        );
    }

    #[tokio::test]
    async fn websocket_connections_are_capped() {
        // Hold every permit, then a new feed connection must be refused before connecting.
        // A private pool of the production size: the process-wide one is shared with every
        // test that opens a feed, and holding all of it made them fail by chance.
        let pool = std::sync::Arc::new(tokio::sync::Semaphore::new(super::MAX_WS_CONNECTIONS));
        let held = pool
            .clone()
            .acquire_many_owned(super::MAX_WS_CONNECTIONS as u32)
            .await
            .unwrap();
        let rpc = crate::rpc::Rpc::new("http://127.0.0.1:9");
        let e = super::ws_loop("ws://127.0.0.1:9", &rpc, &super::new_state(), &mut 0, &pool)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("cap reached"), "{e}");
        drop(held);
        assert_eq!(pool.available_permits(), super::MAX_WS_CONNECTIONS);
    }

    #[test]
    fn staleness_needs_a_recent_websocket_notification() {
        const S: i128 = 1_000_000_000;
        let st = super::new_state();
        let t0 = 1_792_800_000 * S;
        super::record(&st, 1, t0, "poll");
        // polling alone never counts as a fresh feed
        let r = st.lock().unwrap().stale_reason(t0).unwrap();
        assert!(r.contains("no WebSocket notification yet"), "{r}");
        super::record(&st, 2, t0, "ws");
        super::record(&st, 3, t0 + S, "poll");
        let s = st.lock().unwrap();
        assert_eq!(s.stale_reason(t0 + super::STALE_AFTER_NS), None);
        let r = s.stale_reason(t0 + super::STALE_AFTER_NS + 1).unwrap();
        assert!(r.contains("2000 ms ago (limit 2000 ms)"), "{r}");
        assert_eq!(s.ws_age_ms(t0 + 1_500_000_000), Some(1_500));
    }

    /// Feed stall: the server sends a few notifications, then goes silent with the socket
    /// still open. The client must give up after the staleness limit, reconnect by itself and
    /// record the notifications of the new connection.
    #[tokio::test]
    async fn stalled_websocket_is_detected_and_reconnected() {
        use futures_util::{SinkExt, StreamExt};
        use std::time::{Duration, Instant};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(u64, Instant)>();
        tokio::spawn(async move {
            for conn in 1u64.. {
                let (sock, _) = listener.accept().await.unwrap();
                tx.send((conn, Instant::now())).unwrap();
                tokio::spawn(async move {
                    let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
                    let _subscribe = ws.next().await;
                    for k in 0..5u64 {
                        let slot = conn * 1_000 + k;
                        let note = serde_json::json!({"jsonrpc": "2.0", "method": "slotNotification",
                            "params": {"result": {"slot": slot, "parent": slot - 1, "root": 0}, "subscription": 1}});
                        ws.send(Message::Text(note.to_string().into()))
                            .await
                            .unwrap();
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    // stall: the socket stays open, nothing more is sent
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    drop(ws);
                });
            }
        });
        let state = super::new_state();
        let rpc = crate::rpc::Rpc::new("http://127.0.0.1:9");
        let h = super::spawn(Some(format!("ws://{addr}")), rpc, state.clone());
        let wait = Duration::from_secs(10);
        let (_, t1) = tokio::time::timeout(wait, rx.recv())
            .await
            .unwrap()
            .unwrap();
        // the stall is detected after ~2 s and followed by an immediate reconnect
        let (n2, t2) = tokio::time::timeout(wait, rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n2, 2);
        let dt = t2 - t1;
        assert!(
            dt >= Duration::from_millis(2_000) && dt < Duration::from_millis(4_500),
            "reconnect after {dt:?}"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
        {
            let s = state.lock().unwrap();
            assert!(
                s.first_seen.contains_key(&1_004),
                "first connection recorded"
            );
            assert!(s.first_seen.contains_key(&2_000), "new connection recorded");
            assert!(s.stale_reason(crate::burst::unix_ns()).is_none());
        }
        h.abort();
    }
}
