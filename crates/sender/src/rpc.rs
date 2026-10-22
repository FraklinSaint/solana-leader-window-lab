//! Minimal JSON-RPC client.
//!
//! - Every request (retries included) waits for a token from one shared bucket
//!   (`ratelimit.rs`, <= 8 req/s sustained); `sendTransaction` also waits for its own 1/s
//!   bucket. Clones share both buckets, so the window loop, the collector and the slot-feed
//!   fallback together stay under the limit.
//! - HTTP 429 and "Too many requests" bodies are retried with exponential backoff
//!   (Retry-After honoured) and logged, since rate limits are LOGBOOK material.
//! - Only methods in [`ALLOWED_METHODS`] can be called: standard Solana JSON-RPC, no
//!   provider extensions (no transactionSubscribe, blockSubscribe, priority-fee APIs ...).
//! - Every call is counted by the credit meter (`credits.rs`).
//! - The endpoint URL carries the API key: it is never logged, and transport errors are
//!   stripped of their URL and redacted.

use crate::credits::CreditMeter;
use crate::ratelimit::{
    FileBucket, Limiter, MAX_RPC_PER_SEC, MAX_SEND_TX_PER_SEC, RPC_BURST, SHARED_RPC,
    SHARED_SEND_TX, TokenBucket,
};
use crate::redact::redact;
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

pub fn init_crypto() {
    // reqwest/quinn use rustls; pick ring explicitly (no aws-lc build dependency).
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Standard Solana JSON-RPC methods this lab uses. Anything else is refused before a
/// request is made (instruction: no paid-only or provider-specific extensions; outcomes
/// come from getBlock and getSignatureStatuses).
pub const ALLOWED_METHODS: &[&str] = &[
    "getBalance",
    "getBlock",
    "getBlockHeight",
    "getClusterNodes",
    "getEpochInfo",
    "getFeeForMessage",
    "getGenesisHash",
    "getLatestBlockhash",
    "getLeaderSchedule",
    "getMinimumBalanceForRentExemption",
    "getMultipleAccounts",
    "getSignatureStatuses",
    "getSlot",
    "getSlotLeaders",
    "getTransaction",
    "getVoteAccounts",
    "requestAirdrop",
    "sendTransaction",
    "simulateTransaction",
];

#[derive(Debug, Clone, PartialEq)]
pub struct RpcLimits {
    pub per_sec: f64,
    pub send_tx_per_sec: f64,
    /// Some(dir): buckets shared with every other process using `dir` (ratelimit.rs).
    pub shared_dir: Option<std::path::PathBuf>,
}

impl Default for RpcLimits {
    fn default() -> Self {
        Self {
            per_sec: MAX_RPC_PER_SEC,
            send_tx_per_sec: MAX_SEND_TX_PER_SEC,
            shared_dir: None,
        }
    }
}

#[derive(Clone)]
pub struct Rpc {
    url: String,
    http: reqwest::Client,
    max_retries: u32,
    bucket: Limiter,
    send_bucket: Limiter,
    meter: CreditMeter,
}

/// Backoff before retry `attempt` (0-based): 250 ms doubling, capped at 8 s, or the
/// server's Retry-After if that is longer.
pub fn backoff(attempt: u32, retry_after: Option<Duration>) -> Duration {
    let exp = Duration::from_millis(250u64 << attempt.min(5)).min(Duration::from_secs(8));
    retry_after.map_or(exp, |r| r.max(exp))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpochInfo {
    pub absolute_slot: u64,
    pub block_height: u64,
    pub epoch: u64,
    pub slot_index: u64,
    pub slots_in_epoch: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterNode {
    pub pubkey: String,
    pub version: Option<String>,
    pub feature_set: Option<u64>,
    pub client_id: Option<String>,
    pub tpu_quic: Option<String>,
    pub shred_version: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoteAccount {
    pub vote_pubkey: String,
    pub node_pubkey: String,
    pub activated_stake: u64,
    pub commission: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VoteAccounts {
    pub current: Vec<VoteAccount>,
    pub delinquent: Vec<VoteAccount>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestBlockhash {
    pub blockhash: String,
    pub last_valid_block_height: u64,
}

impl Rpc {
    /// Client with the default limits (8 req/s, sendTransaction 1/s) and its own meter.
    pub fn new(url: &str) -> Self {
        Self::with_limits(url, RpcLimits::default(), CreditMeter::default())
    }

    /// Limits above the defaults are refused: the CLI value can only lower them.
    pub fn with_limits(url: &str, limits: RpcLimits, meter: CreditMeter) -> Self {
        init_crypto();
        assert!(
            limits.per_sec > 0.0 && limits.per_sec <= MAX_RPC_PER_SEC,
            "rpc rate must be in (0, {MAX_RPC_PER_SEC}]"
        );
        assert!(
            limits.send_tx_per_sec > 0.0 && limits.send_tx_per_sec <= MAX_SEND_TX_PER_SEC,
            "sendTransaction rate must be in (0, {MAX_SEND_TX_PER_SEC}]"
        );
        Self {
            url: url.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("http client"),
            max_retries: 8,
            bucket: match &limits.shared_dir {
                Some(d) => {
                    Limiter::Shared(FileBucket::new(d, SHARED_RPC, limits.per_sec, RPC_BURST))
                }
                None => Limiter::Local(TokenBucket::new(limits.per_sec, RPC_BURST)),
            },
            send_bucket: match &limits.shared_dir {
                Some(d) => Limiter::Shared(FileBucket::new(
                    d,
                    SHARED_SEND_TX,
                    limits.send_tx_per_sec,
                    1.0,
                )),
                None => Limiter::Local(TokenBucket::new(limits.send_tx_per_sec, 1.0)),
            },
            meter,
        }
    }

    /// The endpoint with its API key redacted (safe to log and to store).
    pub fn endpoint(&self) -> String {
        redact(&self.url).into_owned()
    }

    pub fn meter(&self) -> &CreditMeter {
        &self.meter
    }

    /// (general req/s, sendTransaction/s, shared across processes?)
    pub fn limits(&self) -> (f64, f64, bool) {
        (
            self.bucket.rate(),
            self.send_bucket.rate(),
            matches!(self.bucket, Limiter::Shared(_)),
        )
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        if !ALLOWED_METHODS.contains(&method) {
            bail!("{method}: not a standard method this lab may call (rpc.rs ALLOWED_METHODS)");
        }
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        for attempt in 0..=self.max_retries {
            if method == "sendTransaction" {
                self.send_bucket.acquire().await;
            }
            self.bucket.acquire().await;
            self.meter.http_call(method);
            let resp = self.http.post(&self.url).json(&body).send().await;
            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    // reqwest errors print the URL, which carries the API key.
                    let e = redact(&e.without_url().to_string()).into_owned();
                    if attempt < self.max_retries {
                        tracing::warn!(method, attempt, error = %e, "rpc transport error, retrying");
                        tokio::time::sleep(backoff(attempt, None)).await;
                        continue;
                    }
                    bail!("{method}: transport error: {e}");
                }
            };
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                self.meter.rate_limited();
                let ra = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.trim().parse::<f64>().ok())
                    .map(Duration::from_secs_f64);
                let wait = backoff(attempt, ra);
                tracing::warn!(
                    method,
                    attempt,
                    wait_ms = wait.as_millis() as u64,
                    "rpc rate limited (429)"
                );
                tokio::time::sleep(wait).await;
                continue;
            }
            let status = resp.status();
            let v: Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => bail!(
                    "{method}: bad json (HTTP {status}): {}",
                    redact(&e.without_url().to_string())
                ),
            };
            if let Some(err) = v.get("error") {
                let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
                if msg.contains("Too many requests") && attempt < self.max_retries {
                    self.meter.rate_limited();
                    let wait = backoff(attempt, None);
                    tracing::warn!(
                        method,
                        attempt,
                        wait_ms = wait.as_millis() as u64,
                        "rpc rate limited (error body)"
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
                bail!("{method}: rpc error {}", redact(&err.to_string()));
            }
            return v
                .get("result")
                .cloned()
                .ok_or_else(|| anyhow!("{method}: no result"));
        }
        bail!("{method}: rate limited after {} retries", self.max_retries)
    }

    async fn typed<T: for<'de> Deserialize<'de>>(&self, method: &str, params: Value) -> Result<T> {
        let v = self.call(method, params).await?;
        serde_json::from_value(v).with_context(|| format!("{method}: unexpected shape"))
    }

    pub async fn epoch_info(&self) -> Result<EpochInfo> {
        self.typed("getEpochInfo", json!([{"commitment": "processed"}]))
            .await
    }
    pub async fn slot(&self) -> Result<u64> {
        self.typed("getSlot", json!([{"commitment": "processed"}]))
            .await
    }
    pub async fn block_height(&self) -> Result<u64> {
        self.typed("getBlockHeight", json!([{"commitment": "processed"}]))
            .await
    }
    pub async fn latest_blockhash(&self) -> Result<LatestBlockhash> {
        let v = self
            .call("getLatestBlockhash", json!([{"commitment": "processed"}]))
            .await?;
        Ok(serde_json::from_value(v["value"].clone())?)
    }
    pub async fn slot_leaders(&self, start: u64, limit: u64) -> Result<Vec<String>> {
        self.typed("getSlotLeaders", json!([start, limit])).await
    }
    pub async fn cluster_nodes(&self) -> Result<Vec<ClusterNode>> {
        self.typed("getClusterNodes", json!([])).await
    }
    pub async fn cluster_nodes_raw(&self) -> Result<Value> {
        self.call("getClusterNodes", json!([])).await
    }
    pub async fn vote_accounts(&self) -> Result<VoteAccounts> {
        self.typed("getVoteAccounts", json!([])).await
    }
    pub async fn min_rent(&self, bytes: usize) -> Result<u64> {
        self.typed("getMinimumBalanceForRentExemption", json!([bytes]))
            .await
    }
    pub async fn balance(&self, pubkey: &str) -> Result<u64> {
        let v = self
            .call("getBalance", json!([pubkey, {"commitment": "confirmed"}]))
            .await?;
        Ok(v["value"].as_u64().unwrap_or(0))
    }
    /// Fee the cluster would charge for this (base64) message; None if the blockhash expired.
    pub async fn fee_for_message(&self, message_b64: &str) -> Result<Option<u64>> {
        let v = self
            .call(
                "getFeeForMessage",
                json!([message_b64, {"commitment": "processed"}]),
            )
            .await?;
        Ok(v["value"].as_u64())
    }
    /// Simulate without signature verification; returns (err, unitsConsumed, logs).
    pub async fn simulate(&self, wire_b64: &str) -> Result<Value> {
        let v = self
            .call(
                "simulateTransaction",
                json!([wire_b64, {"encoding": "base64", "sigVerify": false,
                       "replaceRecentBlockhash": true, "commitment": "processed"}]),
            )
            .await?;
        Ok(v["value"].clone())
    }
    pub async fn signature_statuses(&self, sigs: &[String], history: bool) -> Result<Vec<Value>> {
        let mut out = Vec::with_capacity(sigs.len());
        for chunk in sigs.chunks(256) {
            let v = self
                .call(
                    "getSignatureStatuses",
                    json!([chunk, {"searchTransactionHistory": history}]),
                )
                .await?;
            out.extend(v["value"].as_array().cloned().unwrap_or_default());
        }
        Ok(out)
    }
    pub async fn transaction(&self, sig: &str) -> Result<Option<Value>> {
        let v = self
            .call(
                "getTransaction",
                json!([sig, {"encoding": "json", "maxSupportedTransactionVersion": 1,
                             "commitment": "confirmed"}]),
            )
            .await?;
        Ok((!v.is_null()).then_some(v))
    }
    /// Full block with rewards. `maxSupportedTransactionVersion` must be 1 post-Alpenglow
    /// (VERIFIED_FACTS F1): blocks contain v1 transactions.
    pub async fn block(&self, slot: u64) -> Result<Value> {
        self.call(
            "getBlock",
            json!([slot, {"encoding": "json", "maxSupportedTransactionVersion": 1,
                          "transactionDetails": "full", "rewards": true,
                          "commitment": "confirmed"}]),
        )
        .await
    }
    pub async fn genesis_hash(&self) -> Result<String> {
        self.typed("getGenesisHash", json!([])).await
    }
    /// Block height at an explicit commitment. Expiry proofs use `finalized`.
    pub async fn block_height_at(&self, commitment: &str) -> Result<u64> {
        self.typed("getBlockHeight", json!([{"commitment": commitment}]))
            .await
    }
    /// Leader schedule of the epoch containing `slot`: identity -> slot indices in the epoch.
    pub async fn leader_schedule(
        &self,
        slot: u64,
    ) -> Result<std::collections::HashMap<String, Vec<u64>>> {
        let v = self.call("getLeaderSchedule", json!([slot])).await?;
        if v.is_null() {
            bail!("getLeaderSchedule: no schedule for the epoch of slot {slot}");
        }
        Ok(serde_json::from_value(v)?)
    }
    /// Lamport balances of many accounts at `confirmed` (0 for missing accounts).
    pub async fn balances(&self, pubkeys: &[String]) -> Result<Vec<u64>> {
        let mut out = Vec::with_capacity(pubkeys.len());
        for chunk in pubkeys.chunks(100) {
            let v = self
                .call(
                    "getMultipleAccounts",
                    json!([chunk, {"commitment": "confirmed", "encoding": "base64",
                                   "dataSlice": {"offset": 0, "length": 0}}]),
                )
                .await?;
            let vals = v["value"].as_array().cloned().unwrap_or_default();
            if vals.len() != chunk.len() {
                bail!(
                    "getMultipleAccounts: {} results for {} keys",
                    vals.len(),
                    chunk.len()
                );
            }
            out.extend(vals.iter().map(|a| a["lamports"].as_u64().unwrap_or(0)));
        }
        Ok(out)
    }
    /// Localnet/devnet only (the caller checks the cluster).
    pub async fn request_airdrop(&self, pubkey: &str, lamports: u64) -> Result<String> {
        self.typed("requestAirdrop", json!([pubkey, lamports]))
            .await
    }
    pub async fn send_transaction(
        &self,
        _auth: &crate::live::LiveAuth,
        wire_b64: &str,
    ) -> Result<String> {
        self.typed(
            "sendTransaction",
            json!([wire_b64, {"encoding": "base64", "skipPreflight": true, "maxRetries": 0}]),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_exponential_capped_and_honours_retry_after() {
        assert_eq!(backoff(0, None), Duration::from_millis(250));
        assert_eq!(backoff(2, None), Duration::from_millis(1000));
        assert_eq!(backoff(9, None), Duration::from_secs(8));
        assert_eq!(
            backoff(0, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        assert_eq!(
            backoff(3, Some(Duration::from_millis(1))),
            Duration::from_secs(2)
        );
    }

    #[tokio::test]
    async fn refuses_non_standard_methods_without_a_request() {
        let rpc = Rpc::new(&format!(
            "http://127.0.0.1:9/?api-key={}",
            "0f3c2b1a-9d8e-4f70-a1b2"
        ));
        for m in [
            "transactionSubscribe",
            "blockSubscribe",
            "getPriorityFeeEstimate",
            "sendBundle",
        ] {
            let e = rpc.call(m, json!([])).await.unwrap_err().to_string();
            assert!(e.contains("not a standard method"), "{e}");
        }
        assert!(rpc.meter().usage().by_method.is_empty());
    }

    #[tokio::test]
    async fn transport_errors_do_not_leak_the_key() {
        let key = "0f3c2b1a-9d8e-4f70-a1b2-c3d4e5f60718";
        let mut rpc = Rpc::new(&format!("http://127.0.0.1:9/?api-key={key}"));
        rpc.max_retries = 0;
        let e = format!("{:#}", rpc.slot().await.unwrap_err());
        assert!(!e.contains(key), "{e}");
        assert!(!rpc.endpoint().contains(key));
        assert_eq!(rpc.meter().usage().by_method["getSlot"].calls, 1);
    }

    #[test]
    #[should_panic(expected = "rpc rate")]
    fn limits_can_only_be_lowered() {
        let _ = Rpc::with_limits(
            "http://127.0.0.1:9",
            RpcLimits {
                per_sec: 20.0,
                send_tx_per_sec: 1.0,
                shared_dir: None,
            },
            CreditMeter::default(),
        );
    }
}
