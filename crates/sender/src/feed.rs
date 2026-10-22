//! Slot feed for the clock model: `slotSubscribe` over WebSocket, with a getSlot polling
//! fallback (logged, since it degrades timing precision). Every observation is the local
//! UNIX time at which we first learned about a slot.

use crate::burst::unix_ns;
use crate::clock::SlotClock;
use crate::rpc::Rpc;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

pub struct SlotState {
    pub clock: SlotClock,
    /// First-seen time per slot (bounded), for predicted-vs-observed reports.
    pub first_seen: BTreeMap<u64, i128>,
    pub source: &'static str,
}

pub type Shared = Arc<Mutex<SlotState>>;

pub fn new_state() -> Shared {
    Arc::new(Mutex::new(SlotState {
        clock: SlotClock::new(3000),
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

async fn ws_loop(ws_url: &str, state: &Shared) -> anyhow::Result<()> {
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url).await?;
    ws.send(Message::Text(
        json!({"jsonrpc":"2.0","id":1,"method":"slotSubscribe"}).to_string().into(),
    ))
    .await?;
    tracing::info!(ws_url, "slotSubscribe connected");
    while let Some(msg) = ws.next().await {
        let t = unix_ns();
        let msg = msg?;
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
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Runs forever: WebSocket when available, polling for 60 s between reconnect attempts.
pub fn spawn(ws_url: Option<String>, rpc: Rpc, state: Shared) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Some(url) = &ws_url {
                if let Err(e) = ws_loop(url, &state).await {
                    tracing::warn!(error = %e, "slot websocket failed; falling back to polling for 60s");
                }
            }
            poll_loop(&rpc, &state, Duration::from_secs(60)).await;
        }
    })
}

/// Derive a WebSocket URL from an HTTP RPC URL (http->ws, https->wss, port+1 for localhost).
pub fn ws_from_http(http: &str) -> String {
    let ws = http.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
    if let Some(rest) = ws.strip_prefix("ws://127.0.0.1:") {
        if let Ok(port) = rest.trim_end_matches('/').parse::<u16>() {
            return format!("ws://127.0.0.1:{}", port + 1);
        }
    }
    ws
}

#[cfg(test)]
mod tests {
    #[test]
    fn ws_url_derivation() {
        assert_eq!(super::ws_from_http("https://api.mainnet-beta.solana.com"), "wss://api.mainnet-beta.solana.com");
        assert_eq!(super::ws_from_http("http://127.0.0.1:8899"), "ws://127.0.0.1:8900");
    }
}
