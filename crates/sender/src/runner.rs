//! Campaign runner: the one loop shared by dry-run and live. Only the final write differs.
//!
//! ```text
//! plan chunk (rolling, overlapping) ──► queue of PlannedWindow
//! per window, prep_ms before the burst:
//!   re-predict send time from the live slot clock (logged next to the plan's prediction)
//!   fresh blockhash ─► pick free payers ─► build + sign all N txs
//!   getFeeForMessage check of the model fee ─► optional simulateTransaction (CU usage)
//!   QUIC pre-warm (live, or dry-run --probe-connect: handshake only, no stream)
//!   live only: ledger reservation of every tx, fsync'ed BEFORE the first byte is written
//!   burst at the re-predicted time (DryRunSink | QuicSink | RpcSink)
//!   live only: sent.jsonl + collector queue
//! between windows: clock check (predicted vs observed first slot), collector poll (bounded)
//! end: drain the collector, write summary.json
//! ```

use crate::burst::{self, BurstTx, DryRunSink, QuicSink, RpcSink, SendRecord, unix_ns};
use crate::clock::Fit;
use crate::cluster::Cluster;
use crate::collect::{self, Collector, Pending, SentTx, Status};
use crate::feed;
use crate::keys::NamedKeypair;
use crate::ledger::{Ledger, Purpose};
use crate::live::LiveAuth;
use crate::planner::{self, PlanConfig, PlanState, PlannedWindow, Target};
use crate::quic::{ConnectInfo, TpuClient, TpuConnection};
use crate::rpc::Rpc;
use crate::txbuild::{self, BuiltTx, TxParams};
use anyhow::{Context, Result};
use base64::Engine;
use serde::Serialize;
use slwlab_common::fees::total_fee;
use solana_hash::Hash;
use solana_pubkey::Pubkey;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    Quic,
    Rpc,
}

pub enum Mode<'a> {
    DryRun {
        probe_connect: bool,
    },
    Live {
        auth: &'a LiveAuth,
        ledger: &'a mut Ledger,
        purpose: Purpose,
    },
}

impl Mode<'_> {
    fn name(&self) -> &'static str {
        match self {
            Mode::DryRun { .. } => "dry_run",
            Mode::Live { .. } => "live",
        }
    }
    fn is_live(&self) -> bool {
        matches!(self, Mode::Live { .. })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RunConfig {
    pub run_id: String,
    pub duration_secs: u64,
    /// Blockhash, signing, fee check and QUIC pre-warm happen this long before the burst.
    pub prep_ms: u64,
    pub chunk_slots: u64,
    /// The next schedule chunk is planned once its start is less than this far ahead.
    pub plan_ahead_secs: u64,
    /// Calibration offset sweep: window k uses `offsets_ms[k % len]`; empty = plan offset.
    pub offsets_ms: Vec<i64>,
    /// Live: every k-th window goes through RPC `sendTransaction` (path comparison arm).
    pub rpc_every: Option<usize>,
    /// Simulate one tx every k windows (0 = never) to check CU usage against the limit.
    pub simulate_every: usize,
    pub selection_ref: Option<String>,
    pub min_clock_obs: usize,
    /// Commit of the frozen pre-registration (`prereg-v1`). Required for confirmatory runs.
    pub prereg: Option<String>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            run_id: "run".into(),
            duration_secs: 600,
            prep_ms: 1_500,
            chunk_slots: 1_000,
            plan_ahead_secs: 300,
            offsets_ms: vec![],
            rpc_every: None,
            simulate_every: 25,
            selection_ref: None,
            // a stable slope needs ~100 notifications (~20 s); see clock.rs
            min_clock_obs: 100,
            prereg: None,
        }
    }
}

/// Fee payers. A payer is reused only after its previous tx is resolved (live), and only if
/// its estimated balance covers the worst-case fee while staying rent-exempt.
pub struct PayerPool {
    payers: Vec<NamedKeypair>,
    next: usize,
    busy: HashSet<String>,
    /// Live only: fetched balance minus every reservation since (a lower bound).
    balance: Option<HashMap<String, u64>>,
    floor: u64,
}

impl PayerPool {
    pub fn new(
        payers: Vec<NamedKeypair>,
        busy: HashSet<String>,
        balance: Option<HashMap<String, u64>>,
        floor: u64,
    ) -> Self {
        Self {
            payers,
            next: 0,
            busy,
            balance,
            floor,
        }
    }

    pub fn len(&self) -> usize {
        self.payers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.payers.is_empty()
    }

    pub fn get(&self, i: usize) -> &NamedKeypair {
        &self.payers[i]
    }

    /// Distinct free payers for `costs` (one each), round-robin. Does not mark them busy.
    pub fn take(&mut self, costs: &[u64]) -> Option<Vec<usize>> {
        let k = self.payers.len();
        let mut got = Vec::with_capacity(costs.len());
        for step in 0..k {
            if got.len() == costs.len() {
                break;
            }
            let i = (self.next + step) % k;
            let pk = self.payers[i].pubkey().to_string();
            if self.busy.contains(&pk) {
                continue;
            }
            if let Some(b) = &self.balance
                && b.get(&pk).copied().unwrap_or(0) < self.floor + costs[got.len()]
            {
                continue;
            }
            got.push(i);
        }
        if got.len() < costs.len() {
            return None;
        }
        self.next = (got[got.len() - 1] + 1) % k;
        Some(got)
    }

    pub fn commit(&mut self, i: usize, cost: u64) {
        let pk = self.payers[i].pubkey().to_string();
        if let Some(b) = &mut self.balance {
            let v = b.entry(pk.clone()).or_default();
            *v = v.saturating_sub(cost);
        }
        self.busy.insert(pk);
    }

    pub fn release(&mut self, pubkey: &str) {
        self.busy.remove(pubkey);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FeeCheck {
    pub model_lamports: u64,
    pub rpc_lamports: Option<u64>,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowLog {
    pub run_id: String,
    pub mode: &'static str,
    pub cluster: String,
    pub ts: String,
    pub window: PlannedWindow,
    pub offset_ms: i64,
    pub repredicted_send_unix_ns: Option<i128>,
    pub fit: Option<Fit>,
    pub blockhash: Option<String>,
    pub last_valid_block_height: Option<u64>,
    pub payers: Vec<String>,
    pub sigs: Vec<String>,
    pub fee_check: Option<FeeCheck>,
    pub simulation: Option<serde_json::Value>,
    pub path: Option<PathKind>,
    pub connect: Option<ConnectInfo>,
    pub connect_error: Option<String>,
    pub reserved_lamports: u64,
    pub sends: Vec<SendRecord>,
    /// "sent" | "dry_run" | "skipped"
    pub outcome: &'static str,
    pub skip_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClockLine {
    pub window_id: String,
    pub first_slot: u64,
    pub predicted_plan_unix_ns: i128,
    pub predicted_prep_unix_ns: Option<i128>,
    pub observed_unix_ns: Option<i128>,
    pub err_plan_ms: Option<f64>,
    pub err_prep_ms: Option<f64>,
    pub source: &'static str,
}

struct ClockWatch {
    window_id: String,
    first_slot: u64,
    predicted_plan: i128,
    predicted_prep: Option<i128>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClockStats {
    pub windows: usize,
    pub observed: usize,
    pub median_abs_err_ms: Option<f64>,
    pub p95_abs_err_ms: Option<f64>,
    pub max_abs_err_ms: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub mode: String,
    pub cluster: String,
    pub started: String,
    pub finished: String,
    pub windows_planned: usize,
    pub windows_executed: usize,
    pub skips: BTreeMap<String, usize>,
    pub txs_sent: usize,
    pub send_errors: usize,
    pub landed: usize,
    pub expired: usize,
    pub unresolved: usize,
    pub spent_run_lamports: Option<u64>,
    pub reserved_run_lamports: Option<u64>,
    pub spent_total_lamports: Option<u64>,
    pub clock_prep: ClockStats,
    pub stopped_early: Option<String>,
}

/// Collapse numbers so skip reasons aggregate ("lead 812 ms < min 3000" -> "lead # ms < min #").
fn reason_key(r: &str) -> String {
    let mut out = String::new();
    let mut in_num = false;
    for ch in r.chars() {
        if ch.is_ascii_digit() {
            if !in_num {
                out.push('#');
            }
            in_num = true;
        } else {
            in_num = false;
            out.push(ch);
        }
    }
    out
}

fn percentile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let i = ((sorted.len() - 1) as f64 * q).round() as usize;
    Some(sorted[i])
}

pub fn clock_stats(errs_ms: &[f64], windows: usize) -> ClockStats {
    let mut a: Vec<f64> = errs_ms.iter().map(|e| e.abs()).collect();
    a.sort_by(|x, y| x.total_cmp(y));
    ClockStats {
        windows,
        observed: a.len(),
        median_abs_err_ms: percentile(&a, 0.5),
        p95_abs_err_ms: percentile(&a, 0.95),
        max_abs_err_ms: a.last().copied(),
    }
}

fn instant_at(target_unix_ns: i128) -> Instant {
    let now_u = unix_ns();
    let now_i = Instant::now();
    if target_unix_ns > now_u {
        now_i + Duration::from_nanos((target_unix_ns - now_u) as u64)
    } else {
        now_i
    }
}

fn ts() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn parse_hash(b58: &str) -> Result<Hash> {
    let bytes: [u8; 32] = bs58::decode(b58)
        .into_vec()?
        .try_into()
        .map_err(|_| anyhow::anyhow!("blockhash {b58} is not 32 bytes"))?;
    Ok(Hash::new_from_array(bytes))
}

/// Deterministic, small, non-zero nonce (self-transfer amount) per window and tx.
pub fn nonce_for(seed: u64, idx: usize) -> u64 {
    1 + (seed.wrapping_add(idx as u64 * 7_919) % 997)
}

pub struct RunnerInit<'a> {
    pub rpc: Rpc,
    pub cluster: Cluster,
    pub data_root: PathBuf,
    pub cfg: RunConfig,
    pub plan_cfg: PlanConfig,
    pub targets: Vec<Target>,
    pub feed: feed::Shared,
    pub pool: PayerPool,
    pub shared_sink: Option<Pubkey>,
    pub mode: Mode<'a>,
}

pub struct Runner<'a> {
    rpc: Rpc,
    cluster: Cluster,
    run_dir: PathBuf,
    cfg: RunConfig,
    plan_cfg: PlanConfig,
    targets: Vec<Target>,
    feed: feed::Shared,
    pool: PayerPool,
    shared_sink: Option<Pubkey>,
    mode: Mode<'a>,
    collector: Collector,
    quic: Option<TpuClient>,
    watches: Vec<ClockWatch>,
    clock_errs: Vec<f64>,
    summary: RunSummary,
    n_windows: usize,
    last_poll: Instant,
    stop: Option<String>,
}

/// Where a run's measurement outcomes go. Dry-runs write none; calibration is exploratory.
pub fn manifest_path(data_root: &Path, purpose: Purpose) -> PathBuf {
    match purpose {
        Purpose::Measure => data_root.join("manifest.jsonl"),
        _ => data_root
            .join("exploratory")
            .join("calibration_manifest.jsonl"),
    }
}

impl<'a> Runner<'a> {
    pub fn new(init: RunnerInit<'a>) -> Result<Self> {
        let run_dir = init.data_root.join("logs").join(&init.cfg.run_id);
        std::fs::create_dir_all(&run_dir)?;
        let manifest = match &init.mode {
            Mode::Live { purpose, .. } => Some(manifest_path(&init.data_root, *purpose)),
            Mode::DryRun { .. } => None,
        };
        let needs_quic = match &init.mode {
            Mode::Live { .. } => true,
            Mode::DryRun { probe_connect } => *probe_connect,
        };
        Ok(Self {
            collector: Collector::new(init.rpc.clone(), &init.data_root, manifest),
            quic: if needs_quic {
                Some(TpuClient::new()?)
            } else {
                None
            },
            summary: RunSummary {
                run_id: init.cfg.run_id.clone(),
                mode: init.mode.name().into(),
                cluster: init.cluster.name(),
                started: ts(),
                ..Default::default()
            },
            rpc: init.rpc,
            cluster: init.cluster,
            run_dir,
            cfg: init.cfg,
            plan_cfg: init.plan_cfg,
            targets: init.targets,
            feed: init.feed,
            pool: init.pool,
            shared_sink: init.shared_sink,
            mode: init.mode,
            watches: vec![],
            clock_errs: vec![],
            n_windows: 0,
            last_poll: Instant::now(),
            stop: None,
        })
    }

    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    fn fit(&self) -> Option<Fit> {
        let s = self.feed.lock().ok()?;
        if s.clock.len() < self.cfg.min_clock_obs {
            return None;
        }
        s.clock.fit()
    }

    fn log<T: Serialize>(&self, file: &str, item: &T) -> Result<()> {
        collect::append_jsonl(&self.run_dir.join(file), std::slice::from_ref(item))
    }

    async fn wait_for_clock(&self) -> Result<Fit> {
        let end = Instant::now() + Duration::from_secs(120);
        loop {
            if let Some(f) = self.fit() {
                return Ok(f);
            }
            anyhow::ensure!(
                Instant::now() < end,
                "slot clock has fewer than {} observations after 120 s (slot feed down?)",
                self.cfg.min_clock_obs
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn run(mut self) -> Result<RunSummary> {
        self.log(
            "run.json",
            &serde_json::json!({
                "run_id": self.cfg.run_id, "mode": self.mode.name(), "cluster": self.cluster.name(),
                "exploratory": !matches!(self.mode, Mode::Live { purpose: Purpose::Measure, .. }),
                "run_config": self.cfg, "plan_config": self.plan_cfg, "targets": self.targets,
                "payers": self.pool.len(), "shared_sink": self.shared_sink.map(|p| p.to_string()),
                "started": self.summary.started,
            }),
        )?;
        self.wait_for_clock().await?;
        let deadline = unix_ns() + i128::from(self.cfg.duration_secs) * 1_000_000_000;
        let mut state = PlanState::default();
        let mut next_start = self.rpc.slot().await? + 10;
        let mut queue: VecDeque<PlannedWindow> = VecDeque::new();
        let mut planning_done = false;
        while self.stop.is_none() {
            while !planning_done {
                let fit = self.fit().context("slot clock lost")?;
                let t = fit.predict_ns(next_start) as i128;
                if t > deadline {
                    planning_done = true;
                    break;
                }
                if t > unix_ns() + i128::from(self.cfg.plan_ahead_secs) * 1_000_000_000 {
                    break;
                }
                let leaders = self
                    .rpc
                    .slot_leaders(next_start, self.cfg.chunk_slots)
                    .await?;
                let (planned, skips) = planner::plan_windows(
                    next_start,
                    &leaders,
                    &self.targets,
                    &fit,
                    unix_ns(),
                    &self.plan_cfg,
                    self.shared_sink,
                    &mut state,
                );
                for s in &skips {
                    *self.summary.skips.entry(reason_key(&s.reason)).or_default() += 1;
                    self.log("plan.jsonl", &serde_json::json!({"skip": s}))?;
                }
                for w in planned
                    .into_iter()
                    .filter(|w| w.send_at_unix_ns <= deadline)
                {
                    self.log("plan.jsonl", &serde_json::json!({"window": w}))?;
                    self.summary.windows_planned += 1;
                    queue.push_back(w);
                }
                next_start += self.cfg.chunk_slots.saturating_sub(16).max(1);
                // Fell behind (slow RPC)? Never plan into the past.
                let now_slot = self.rpc.slot().await?;
                next_start = next_start.max(now_slot + 10);
            }
            match queue.pop_front() {
                Some(w) => self.execute(w).await?,
                None if planning_done => break,
                None => self.idle_until(unix_ns() + 1_000_000_000).await?,
            }
        }
        if self.mode.is_live() {
            let end = Instant::now() + Duration::from_secs(240);
            while self.collector.pending_len() > 0 && Instant::now() < end {
                self.poll_collector(Duration::from_secs(60)).await?;
                self.check_clock()?;
                if self.collector.pending_len() > 0 {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        } else {
            // let the last windows' first slots be observed for the clock report
            let end = Instant::now() + Duration::from_secs(15);
            while !self.watches.is_empty() && Instant::now() < end {
                self.check_clock()?;
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        self.check_clock()?;
        self.summary.unresolved = self.collector.pending_len();
        self.summary.clock_prep = clock_stats(&self.clock_errs, self.summary.windows_executed);
        self.summary.stopped_early = self.stop.clone();
        if let Mode::Live { ledger, .. } = &self.mode {
            let t = ledger.totals();
            self.summary.spent_run_lamports = Some(t.spent_run);
            self.summary.reserved_run_lamports = Some(t.reserved_run);
            self.summary.spent_total_lamports = Some(t.spent_total);
        }
        self.summary.finished = ts();
        std::fs::write(
            self.run_dir.join("summary.json"),
            serde_json::to_vec_pretty(&self.summary)?,
        )?;
        Ok(self.summary)
    }

    async fn idle_until(&mut self, t: i128) -> Result<()> {
        loop {
            let rem_ms = ((t - unix_ns()) / 1_000_000) as i64;
            if rem_ms <= 0 {
                return Ok(());
            }
            self.check_clock()?;
            if rem_ms > 3_000
                && self.collector.pending_len() > 0
                && self.last_poll.elapsed() > Duration::from_secs(3)
            {
                self.poll_collector(Duration::from_millis((rem_ms - 2_000) as u64))
                    .await?;
                continue;
            }
            tokio::time::sleep(Duration::from_millis(rem_ms.min(200) as u64)).await;
        }
    }

    /// Bounded collector pass. RPC trouble is logged, not fatal; ledger errors are fatal.
    async fn poll_collector(&mut self, budget: Duration) -> Result<()> {
        self.last_poll = Instant::now();
        let obs = match tokio::time::timeout(budget, self.collector.observe()).await {
            Err(_) => {
                tracing::warn!(
                    budget_ms = budget.as_millis() as u64,
                    "collector pass timed out"
                );
                return Ok(());
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %format!("{e:#}"), "collector pass failed");
                return Ok(());
            }
            Ok(Ok(o)) => o,
        };
        let ledger = match &mut self.mode {
            Mode::Live { ledger, .. } => Some(&mut **ledger),
            Mode::DryRun { .. } => None,
        };
        let lines = self.collector.apply(&obs, ledger)?;
        for l in &lines {
            self.pool.release(&l.tx.payer);
            match l.outcome.status {
                Status::Landed => self.summary.landed += 1,
                Status::Expired => self.summary.expired += 1,
            }
        }
        Ok(())
    }

    fn check_clock(&mut self) -> Result<()> {
        if self.watches.is_empty() {
            return Ok(());
        }
        let (lines, keep) = {
            let s = self
                .feed
                .lock()
                .map_err(|_| anyhow::anyhow!("slot state poisoned"))?;
            let latest = s.first_seen.last_key_value().map(|(k, _)| *k);
            let mut lines = vec![];
            let mut keep = vec![];
            for w in self.watches.drain(..) {
                let observed = s.first_seen.get(&w.first_slot).copied();
                let gone = latest.is_some_and(|l| l > w.first_slot + 20);
                if observed.is_none() && !gone {
                    keep.push(w);
                    continue;
                }
                let err = |p: i128| observed.map(|o| (o - p) as f64 / 1e6);
                lines.push(ClockLine {
                    err_plan_ms: err(w.predicted_plan),
                    err_prep_ms: w.predicted_prep.and_then(err),
                    window_id: w.window_id,
                    first_slot: w.first_slot,
                    predicted_plan_unix_ns: w.predicted_plan,
                    predicted_prep_unix_ns: w.predicted_prep,
                    observed_unix_ns: observed,
                    source: s.source,
                });
            }
            (lines, keep)
        };
        self.watches = keep;
        for l in &lines {
            if let Some(e) = l.err_prep_ms {
                self.clock_errs.push(e);
            }
        }
        collect::append_jsonl(&self.run_dir.join("clock.jsonl"), &lines)
    }

    fn skip(&mut self, mut log: WindowLog, reason: String) -> Result<()> {
        tracing::info!(window = %log.window.window_id, %reason, "window skipped");
        *self.summary.skips.entry(reason_key(&reason)).or_default() += 1;
        log.outcome = "skipped";
        log.skip_reason = Some(reason);
        self.log("windows.jsonl", &log)
    }

    async fn connect(&self, addr: &str) -> Result<TpuConnection> {
        let quic = self.quic.as_ref().context("no QUIC client")?;
        let addr: SocketAddr = addr.parse().with_context(|| format!("tpuQuic {addr}"))?;
        tokio::time::timeout(Duration::from_millis(1_000), quic.connect(addr))
            .await
            .context("QUIC handshake timed out (1 s)")?
    }

    async fn execute(&mut self, w: PlannedWindow) -> Result<()> {
        let k = self.n_windows;
        self.n_windows += 1;
        let offset_ms = if self.cfg.offsets_ms.is_empty() {
            self.plan_cfg.offset_ms
        } else {
            self.cfg.offsets_ms[k % self.cfg.offsets_ms.len()]
        };
        let shift = i128::from(offset_ms - self.plan_cfg.offset_ms) * 1_000_000;
        let prep_ns = i128::from(self.cfg.prep_ms) * 1_000_000;
        self.idle_until(w.send_at_unix_ns + shift - prep_ns).await?;

        let fit = self.fit();
        let predicted_prep = fit.map(|f| f.predict_ns(w.first_slot) as i128);
        let send_at = predicted_prep.unwrap_or(w.predicted_first_slot_unix_ns)
            + i128::from(offset_ms) * 1_000_000;
        self.watches.push(ClockWatch {
            window_id: w.window_id.clone(),
            first_slot: w.first_slot,
            predicted_plan: w.predicted_first_slot_unix_ns,
            predicted_prep,
        });
        let mut log = WindowLog {
            run_id: self.cfg.run_id.clone(),
            mode: self.mode.name(),
            cluster: self.cluster.name(),
            ts: ts(),
            window: w.clone(),
            offset_ms,
            repredicted_send_unix_ns: Some(send_at),
            fit,
            blockhash: None,
            last_valid_block_height: None,
            payers: vec![],
            sigs: vec![],
            fee_check: None,
            simulation: None,
            path: None,
            connect: None,
            connect_error: None,
            reserved_lamports: 0,
            sends: vec![],
            outcome: "skipped",
            skip_reason: None,
        };
        let margin_ms = (send_at - unix_ns()) / 1_000_000;
        if margin_ms < 300 {
            return self.skip(
                log,
                format!("late after re-prediction: {margin_ms} ms left"),
            );
        }

        let bh = match self.rpc.latest_blockhash().await {
            Ok(b) => b,
            Err(e) => return self.skip(log, format!("getLatestBlockhash failed: {e:#}")),
        };
        let hash = parse_hash(&bh.blockhash)?;
        log.blockhash = Some(bh.blockhash.clone());
        log.last_valid_block_height = Some(bh.last_valid_block_height);

        let lps = self.plan_cfg.lamports_per_signature;
        let d = &w.design;
        let costs: Vec<u64> = d
            .txs
            .iter()
            .map(|t| total_fee(lps, 1, t.cu_price_micro_lamports, t.cu_limit))
            .collect();
        let Some(pidx) = self.pool.take(&costs) else {
            return self.skip(log, "payer pool exhausted (busy or underfunded)".into());
        };
        let mut built: Vec<BuiltTx> = Vec::with_capacity(d.txs.len());
        for (t, &pi) in d.txs.iter().zip(&pidx) {
            built.push(txbuild::build(
                &self.pool.get(pi).keypair,
                &hash,
                &TxParams {
                    cu_limit: t.cu_limit,
                    cu_price_micro_lamports: t.cu_price_micro_lamports,
                    nonce: nonce_for(d.seed, t.idx),
                    shared_sink: d.shared_sink,
                },
            )?);
        }
        log.payers = pidx
            .iter()
            .map(|&i| self.pool.get(i).pubkey().to_string())
            .collect();
        log.sigs = built.iter().map(|b| b.signature.to_string()).collect();

        // The model fee must match what the cluster would charge (checked on the dearest tx).
        let kmax = (0..costs.len()).max_by_key(|&i| costs[i]).unwrap_or(0);
        let rpc_fee = self
            .rpc
            .fee_for_message(&b64(&built[kmax].message))
            .await
            .ok()
            .flatten();
        let fee_ok = rpc_fee == Some(costs[kmax]);
        log.fee_check = Some(FeeCheck {
            model_lamports: costs[kmax],
            rpc_lamports: rpc_fee,
            ok: fee_ok,
        });
        if !fee_ok {
            tracing::warn!(model = costs[kmax], rpc = ?rpc_fee, "fee model check failed");
            if self.mode.is_live() {
                return self.skip(log, "getFeeForMessage disagrees with the fee model".into());
            }
        }

        if self.cfg.simulate_every > 0 && k.is_multiple_of(self.cfg.simulate_every) {
            let sim = self.rpc.simulate(&b64(&built[kmax].wire)).await;
            let units = sim.as_ref().ok().and_then(|v| v["unitsConsumed"].as_u64());
            log.simulation = Some(match &sim {
                Ok(v) => serde_json::json!({"err": v["err"], "units_consumed": units,
                                            "cu_limit": d.txs[kmax].cu_limit}),
                Err(e) => serde_json::json!({"rpc_error": format!("{e:#}")}),
            });
            if self.mode.is_live()
                && let (Ok(v), Some(u)) = (&sim, units)
                && v["err"].is_null()
                && u >= u64::from(d.txs[kmax].cu_limit)
            {
                self.stop = Some(format!(
                    "CU limit {} does not exceed measured usage {u}",
                    d.txs[kmax].cu_limit
                ));
                return self.skip(log, "CU limit below usage".into());
            }
        }

        let path = match &self.mode {
            Mode::Live { .. } => Some(match self.cfg.rpc_every {
                Some(e) if e > 0 && k % e == e - 1 => PathKind::Rpc,
                _ => PathKind::Quic,
            }),
            Mode::DryRun { .. } => None,
        };
        log.path = path;
        let wants_conn = match &self.mode {
            Mode::Live { .. } => path == Some(PathKind::Quic),
            Mode::DryRun { probe_connect } => *probe_connect,
        };
        let mut conn = None;
        if wants_conn {
            match w.tpu_quic.as_deref() {
                None => log.connect_error = Some("no tpuQuic".into()),
                Some(a) => match self.connect(a).await {
                    Ok(c) => {
                        log.connect = Some(c.info.clone());
                        conn = Some(c);
                    }
                    Err(e) => log.connect_error = Some(format!("{e:#}")),
                },
            }
        }

        let burst_txs: Vec<BurstTx> = d
            .txs
            .iter()
            .zip(&built)
            .map(|(t, b)| BurstTx {
                idx: t.idx,
                sig: b.signature.to_string(),
                wire: b.wire.clone(),
                offset_us: t.send_offset_us,
            })
            .collect();

        match &mut self.mode {
            Mode::DryRun { .. } => {
                if let Some(c) = &conn {
                    c.close(); // probe only: no stream was ever opened
                }
                let mut sink = DryRunSink::default();
                log.sends = burst::run_burst(&mut sink, &burst_txs, instant_at(send_at)).await;
                log.outcome = "dry_run";
                self.summary.windows_executed += 1;
                self.log("windows.jsonl", &log)?;
                return Ok(());
            }
            Mode::Live {
                auth,
                ledger,
                purpose,
            } => {
                if path == Some(PathKind::Quic) && conn.is_none() {
                    let reason = format!(
                        "QUIC connect failed: {}",
                        log.connect_error.clone().unwrap_or_default()
                    );
                    return self.skip(log, reason);
                }
                let total: u64 = costs.iter().sum();
                if let Err(e) = ledger.check(total) {
                    self.stop = Some(e.to_string());
                    return self.skip(log, format!("spend cap: {e}"));
                }
                // Write-ahead: every reservation is durable before any byte is written.
                for (i, b) in built.iter().enumerate() {
                    ledger.reserve_paid_by(
                        &b.signature.to_string(),
                        costs[i],
                        bh.last_valid_block_height,
                        *purpose,
                        Some(&w.window_id),
                        Some(&log.payers[i]),
                    )?;
                }
                log.reserved_lamports = total;
                for (i, &pi) in pidx.iter().enumerate() {
                    self.pool.commit(pi, costs[i]);
                }
                let auth: &LiveAuth = auth;
                let purpose = *purpose;
                let start = instant_at(send_at);
                let sends = match (&conn, path) {
                    (Some(c), Some(PathKind::Quic)) => {
                        let mut sink = QuicSink::new(c, auth);
                        match burst::Sink::prepare(&mut sink, burst_txs.len()).await {
                            Ok(()) => burst::run_burst(&mut sink, &burst_txs, start).await,
                            Err(e) => failed_sends(&burst_txs, "quic", &format!("{e:#}")),
                        }
                    }
                    _ => {
                        let mut sink = RpcSink::new(&self.rpc, auth);
                        burst::run_burst(&mut sink, &burst_txs, start).await
                    }
                };
                if let Some(c) = &conn {
                    // Let the last stream flush before closing the connection.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    c.close();
                }
                let by_idx: HashMap<usize, &SendRecord> =
                    sends.iter().map(|r| (r.idx, r)).collect();
                let mut sent = vec![];
                for (i, (t, b)) in d.txs.iter().zip(&built).enumerate() {
                    let rec = by_idx
                        .get(&t.idx)
                        .map(|r| (*r).clone())
                        .context("send record")?;
                    if rec.error.is_some() {
                        self.summary.send_errors += 1;
                    }
                    sent.push(SentTx {
                        sig: b.signature.to_string(),
                        run_id: self.cfg.run_id.clone(),
                        cluster: self.cluster.name(),
                        purpose,
                        window_id: w.window_id.clone(),
                        identity: w.identity.clone(),
                        vote_account: w.vote_account.clone(),
                        stratum: w.stratum.clone(),
                        selection_ref: self.cfg.selection_ref.clone(),
                        arm: d.arm,
                        gap_us: d.gap_us,
                        offset_ms,
                        first_slot: w.first_slot,
                        n_slots: w.n_slots,
                        idx: t.idx,
                        send_rank: t.send_rank,
                        payer: log.payers[i].clone(),
                        cu_limit: t.cu_limit,
                        cu_price_micro_lamports: t.cu_price_micro_lamports,
                        n_signatures: 1,
                        conflict_account: d.shared_sink.map(|p| p.to_string()),
                        reserved_lamports: costs[i],
                        blockhash: bh.blockhash.clone(),
                        last_valid_block_height: bh.last_valid_block_height,
                        planned_send_unix_ns: send_at + i128::from(t.send_offset_us) * 1_000,
                        send: rec,
                    });
                }
                collect::append_jsonl(&self.run_dir.join("sent.jsonl"), &sent)?;
                for s in sent {
                    self.collector.add(Pending {
                        sig: s.sig.clone(),
                        last_valid_block_height: s.last_valid_block_height,
                        sent: Some(s),
                    });
                }
                self.summary.txs_sent += burst_txs.len();
                self.summary.windows_executed += 1;
                log.sends = sends;
                log.outcome = "sent";
                self.log("windows.jsonl", &log)?;
            }
        }
        Ok(())
    }
}

/// Records for a burst that could not start (e.g. streams could not be opened). Their
/// reservations stay until the collector proves expiry.
fn failed_sends(txs: &[BurstTx], path: &str, err: &str) -> Vec<SendRecord> {
    txs.iter()
        .map(|t| SendRecord {
            idx: t.idx,
            sig: t.sig.clone(),
            path: path.into(),
            planned_offset_us: t.offset_us,
            start_mono_ns: 0,
            end_mono_ns: 0,
            start_unix_ns: unix_ns(),
            end_unix_ns: unix_ns(),
            stream_id: None,
            conn_id: None,
            rtt_us: None,
            error: Some(err.into()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::ephemeral_payers;

    #[test]
    fn pool_round_robin_skips_busy_and_underfunded() {
        let payers = ephemeral_payers(4);
        let pks: Vec<String> = payers.iter().map(|p| p.pubkey().to_string()).collect();
        let balance = HashMap::from([
            (pks[0].clone(), 10_000),
            (pks[1].clone(), 10_000),
            (pks[2].clone(), 100), // underfunded
            (pks[3].clone(), 10_000),
        ]);
        let mut pool = PayerPool::new(payers, HashSet::new(), Some(balance), 1_000);
        let got = pool.take(&[5_000, 5_000]).unwrap();
        assert_eq!(got, vec![0, 1]);
        pool.commit(0, 5_000);
        pool.commit(1, 5_000);
        // 0 and 1 are busy, 2 is underfunded: only 3 is left
        assert!(pool.take(&[5_000, 5_000]).is_none());
        assert_eq!(pool.take(&[5_000]).unwrap(), vec![3]);
        // released, but its balance estimate is now 5_000 < 1_000 + 5_000
        pool.release(&pks[0]);
        assert!(pool.take(&[5_000, 5_000]).is_none());
        assert_eq!(pool.take(&[4_000]).unwrap(), vec![0]);
    }

    #[test]
    fn dry_run_pool_has_no_balance_gate() {
        let mut pool = PayerPool::new(ephemeral_payers(8), HashSet::new(), None, 650_240);
        assert_eq!(pool.take(&[6_000; 8]).unwrap().len(), 8);
    }

    #[test]
    fn nonces_are_small_nonzero_and_distinct_within_a_window() {
        for seed in [0, 1, u64::MAX, 123_456_789] {
            let v: HashSet<u64> = (0..8).map(|i| nonce_for(seed, i)).collect();
            assert_eq!(v.len(), 8);
            assert!(v.iter().all(|&n| (1..=997).contains(&n)));
        }
    }

    #[test]
    fn reason_keys_aggregate() {
        assert_eq!(reason_key("lead 812 ms < min 3000"), "lead # ms < min #");
        assert_eq!(reason_key("payer pool exhausted"), "payer pool exhausted");
    }

    #[test]
    fn clock_stats_use_absolute_errors() {
        let s = clock_stats(&[-10.0, 2.0, 4.0, -1.0], 5);
        assert_eq!((s.windows, s.observed), (5, 4));
        assert_eq!(s.max_abs_err_ms, Some(10.0));
        assert_eq!(s.median_abs_err_ms, Some(4.0));
    }

    #[test]
    fn calibration_and_measurement_manifests_are_separate() {
        let root = Path::new("data");
        assert_eq!(
            manifest_path(root, Purpose::Measure),
            root.join("manifest.jsonl")
        );
        assert!(manifest_path(root, Purpose::Calibrate).starts_with(root.join("exploratory")));
    }
}
