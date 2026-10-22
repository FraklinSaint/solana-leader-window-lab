//! Minimal JSON-RPC client. Retries rate limits (HTTP 429 and the public RPC's
//! "Too many requests" error) with linear backoff and logs each retry, since rate limits are
//! LOGBOOK material.

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

pub fn init_crypto() {
    // reqwest/quinn use rustls; pick ring explicitly (no aws-lc build dependency).
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[derive(Clone)]
pub struct Rpc {
    url: String,
    http: reqwest::Client,
    max_retries: u32,
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
    pub fn new(url: &str) -> Self {
        init_crypto();
        Self {
            url: url.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("http client"),
            max_retries: 8,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        for attempt in 0..=self.max_retries {
            let resp = self.http.post(&self.url).json(&body).send().await;
            let resp = match resp {
                Ok(r) => r,
                Err(e) if attempt < self.max_retries => {
                    tracing::warn!(method, attempt, error = %e, "rpc transport error, retrying");
                    tokio::time::sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
                    continue;
                }
                Err(e) => return Err(e).context(method.to_string()),
            };
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                tracing::warn!(method, attempt, "rpc rate limited (429)");
                tokio::time::sleep(Duration::from_millis(1000 * (attempt as u64 + 1))).await;
                continue;
            }
            let v: Value = resp
                .json()
                .await
                .with_context(|| format!("{method}: bad json"))?;
            if let Some(err) = v.get("error") {
                let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
                if msg.contains("Too many requests") && attempt < self.max_retries {
                    tracing::warn!(method, attempt, "rpc rate limited (error body)");
                    tokio::time::sleep(Duration::from_millis(1000 * (attempt as u64 + 1))).await;
                    continue;
                }
                bail!("{method}: rpc error {err}");
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
