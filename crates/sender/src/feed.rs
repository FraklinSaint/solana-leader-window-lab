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

const WS_READ_TIMEOUT: Duration = Duration::from_secs(10);

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
    }))
}

fn record(state: &Shared, slot: u64, t: i128, source: &'static str) {
    let mut s = state.lock().expect("slot state");
    s.source = source;
    s.first_seen.entry(slot).or_insert(t);
    while s.first_seen.len() > 20_000 {
        s.first_seen.pop_first();
    }
    s.clock.observe(slot, t);
}

async fn ws_loop(ws_url: &str, rpc: &Rpc, state: &Shared) -> anyhow::Result<()> {
    let _permit = WS_PERMITS.clone().try_acquire_owned().map_err(|_| {
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
    // A socket can die silently (host sleep, NAT timeout): with ~5 notifications/s, 10 s of
    // silence means it is gone. Bail out so the caller reconnects.
    loop {
        let msg = match tokio::time::timeout(WS_READ_TIMEOUT, ws.next()).await {
            Err(_) => anyhow::bail!("no message for {} s", WS_READ_TIMEOUT.as_secs()),
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

/// Runs forever: WebSocket when available (public endpoints close it every few minutes, so
/// it reconnects after 5 s of polling), otherwise getSlot polling.
pub fn spawn(ws_url: Option<String>, rpc: Rpc, state: Shared) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Some(url) = &ws_url
                && let Err(e) = ws_loop(url, &rpc, &state).await
            {
                tracing::warn!(error = %redact(&format!("{e:#}")), "slot websocket failed; polling for 5 s, then reconnecting");
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
        let held = super::WS_PERMITS
            .clone()
            .acquire_many_owned(super::MAX_WS_CONNECTIONS as u32)
            .await
            .unwrap();
        let rpc = crate::rpc::Rpc::new("http://127.0.0.1:9");
        let e = super::ws_loop("ws://127.0.0.1:9", &rpc, &super::new_state())
            .await
            .unwrap_err();
        assert!(e.to_string().contains("cap reached"), "{e}");
        drop(held);
        assert_eq!(super::ws_open(), 0);
    }
}
