//! Campaign runner: the one loop shared by dry-run and live. Only the final write differs.
//!
//! ```text
//! plan chunk (rolling, overlapping) ──► queue of PlannedWindow
//! per window, prep_ms before the burst:
//!   re-predict send time from the live slot clock (logged next to the plan's prediction)
//!   staleness gate: skip if the WebSocket slot feed is silent > feed::STALE_AFTER_NS
//!   fresh blockhash ─► pick free payers ─► build + sign all N txs
//!   getFeeForMessage check of the model fee ─► optional simulateTransaction (CU usage)
//!   QUIC pre-warm (live, or dry-run --probe-connect: handshake only, no stream)
//!   staleness gate again (the feed may have died during prep)
//!   live only: ledger reservation of every tx, fsync'ed BEFORE the first byte is written
//!   burst at the re-predicted time (DryRunSink | QuicSink | RpcSink)
//!   live only: sent.jsonl + collector queue
//! between windows: clock check (predicted vs observed first slot), collector poll (bounded)
//! end: drain the collector, write summary.json
//! ```

use crate::arms::Arm;
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
    /// Name of the RPC send path in the manifest (`SendRecord.path`).
    pub rpc_path_label: &'static str,
    /// RPC endpoint with the API key redacted (provenance in run.json).
    pub endpoint: String,
    pub clock: crate::clock::ClockParams,
    /// Sending host (host.rs): run.json header and every manifest row.
    pub host: Option<crate::host::HostInfo>,
    /// The session's events.jsonl when run under `sender session` (session::EVENTS_ENV).
    #[serde(skip)]
    pub session_events: Option<PathBuf>,
}

/// `stopped_early` of a run ended by Ctrl+C.
pub const INTERRUPTED: &str = "interrupted (Ctrl+C)";

/// Manifest name of the RPC comparison path. Helius routes `sendTransaction` through its
/// staked connections by default (docs: "Staked Connections (default)"; stated for paid
/// plans, unstated for the free plan, which calibration checks). Other endpoints: "rpc".
pub fn rpc_path_label(rpc_url: &str) -> &'static str {
    let host = rpc_url
        .split("://")
        .nth(1)
        .unwrap_or(rpc_url)
        .split(['/', '?', ':'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if host == "helius-rpc.com" || host.ends_with(".helius-rpc.com") {
        "rpc-helius-staked"
    } else {
        "rpc"
    }
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
            rpc_path_label: "rpc",
            endpoint: String::new(),
            clock: crate::clock::ClockParams::default(),
            host: None,
            session_events: None,
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
    /// Age of the newest WebSocket slot notification at the last staleness check (ms).
    pub feed_age_ms: Option<i64>,
}

/// An open slot-feed staleness episode (one `feed_stale` event, one `feed_recovered`).
#[derive(Debug, Clone)]
struct StaleEpisode {
    since_unix_ns: i128,
    skipped: usize,
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
    /// Landing result per send path ("quic", "rpc-helius-staked", ...). For the RPC path
    /// this is the calibration check of whether the provider's free plan delivers at all.
    pub by_path: BTreeMap<String, PathStats>,
    /// Estimated provider credits used by this process so far (credits.rs).
    pub credits: Option<crate::credits::Usage>,
    pub month_to_date_credits: Option<f64>,
    /// simulateTransaction results per arm: CU consumed and loaded-accounts data size against
    /// the tx's limits (at least one simulation per arm per run).
    pub simulation_by_arm: BTreeMap<String, SimArmStats>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SimArmStats {
    pub simulated: usize,
    /// Simulations that ran without error (an unfunded dry-run payer gives AccountNotFound).
    pub ok: usize,
    pub errors: BTreeMap<String, usize>,
    pub units_min: Option<u64>,
    pub units_max: Option<u64>,
    /// Smallest CU limit among the simulated txs (the binding one).
    pub cu_limit_min: Option<u32>,
    pub loaded_bytes_max: Option<u64>,
    pub loaded_bytes_limit: u32,
    /// Every ok simulation used less than its CU limit and its loaded-data limit.
    pub within_limits: Option<bool>,
}

impl SimArmStats {
    pub fn record(
        &mut self,
        cu_limit: u32,
        units: Option<u64>,
        loaded: Option<u64>,
        err: &serde_json::Value,
    ) {
        self.simulated += 1;
        self.loaded_bytes_limit = crate::txbuild::LOADED_ACCOUNTS_DATA_LIMIT;
        if !err.is_null() {
            let key = err
                .as_str()
                .map(str::to_string)
                .or_else(|| err.as_object().and_then(|o| o.keys().next().cloned()))
                .unwrap_or_else(|| err.to_string());
            *self.errors.entry(key).or_default() += 1;
            return;
        }
        self.ok += 1;
        if let Some(u) = units {
            self.units_min = Some(self.units_min.map_or(u, |m| m.min(u)));
            self.units_max = Some(self.units_max.map_or(u, |m| m.max(u)));
        }
        self.cu_limit_min = Some(self.cu_limit_min.map_or(cu_limit, |m| m.min(cu_limit)));
        if let Some(l) = loaded {
            self.loaded_bytes_max = Some(self.loaded_bytes_max.map_or(l, |m| m.max(l)));
        }
        let fits = units.is_none_or(|u| u < u64::from(cu_limit))
            && loaded.is_none_or(|l| l < u64::from(self.loaded_bytes_limit));
        self.within_limits = Some(self.within_limits.unwrap_or(true) && fits);
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PathStats {
    pub sent: usize,
    pub send_errors: usize,
    pub landed: usize,
    pub expired: usize,
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

fn unix_ns_utc(t: i128) -> String {
    crate::session::utc((t / 1_000_000_000) as i64)
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
    /// Allocation already attempted by earlier runs of this purpose ([`prior_plan_state`]).
    pub prior: PlanState,
    /// Set by the first Ctrl+C: stop planning, drop windows not yet started, write summary.json.
    pub interrupt: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

pub struct Runner<'a> {
    rpc: Rpc,
    cluster: Cluster,
    data_root: PathBuf,
    run_dir: PathBuf,
    cfg: RunConfig,
    plan_cfg: PlanConfig,
    prior: PlanState,
    targets: Vec<Target>,
    feed: feed::Shared,
    pool: PayerPool,
    shared_sink: Option<Pubkey>,
    mode: Mode<'a>,
    collector: Collector,
    feed_stale: Option<StaleEpisode>,
    quic: Option<TpuClient>,
    watches: Vec<ClockWatch>,
    clock_errs: Vec<f64>,
    clock_resolved: usize,
    summary: RunSummary,
    n_windows: usize,
    last_poll: Instant,
    stop: Option<String>,
    watchdog: Option<tokio::task::JoinHandle<()>>,
    last_hourly: Instant,
    slots_dumped_through: u64,
    epoch_map: Option<crate::leaders::EpochMap>,
    /// Epochs whose leader schedule is known to be in data/snapshots (leaders.rs).
    schedules_saved: std::collections::BTreeSet<u64>,
    /// Windows skipped before any reservation; their quota goes back to the plan state.
    released: Vec<PlannedWindow>,
    interrupt: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Arms simulated at least once in this run.
    simulated_arms: std::collections::BTreeSet<Arm>,
}

/// What earlier runs of `purpose` already attempted, so that a new session, or a restart
/// after a crash or power loss, continues the allocation instead of starting it over and never
/// plans a window at or before one already attempted.
///
/// Source: the spend ledger (each reservation is fsync'd before its tx is sent), joined with
/// the run's `logs/<run_id>/plan.jsonl` (identity, arm, first slot; written at plan time). A
/// window counts once however many of its txs were reserved. `sent.jsonl` is not used: it is
/// written after the burst, so a crash in between would undercount. A torn last ledger line
/// (power loss mid-write) is ignored: its tx was never sent. A reserved window missing from its
/// plan.jsonl is an error (deleted logs would silently reset the allocation).
pub fn prior_plan_state(data_root: &Path, ledger: &Path, purpose: Purpose) -> Result<PlanState> {
    let mut state = PlanState::default();
    let Ok(text) = std::fs::read_to_string(ledger) else {
        return Ok(state);
    };
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut windows: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        Default::default();
    for (i, l) in lines.iter().enumerate() {
        if l.trim().is_empty() {
            continue;
        }
        let e: crate::ledger::Entry = match serde_json::from_str(l.trim_end()) {
            Ok(e) => e,
            Err(_) if i + 1 == lines.len() && !l.ends_with('\n') => continue,
            Err(e) => anyhow::bail!("{}: line {}: {e}", ledger.display(), i + 1),
        };
        if let crate::ledger::Entry::Reserve {
            run_id,
            purpose: p,
            window_id: Some(w),
            ..
        } = e
            && p == purpose
        {
            windows.entry(run_id).or_default().insert(w);
        }
    }
    for (run_id, wids) in windows {
        let plan = data_root.join("logs").join(&run_id).join("plan.jsonl");
        let planned: std::collections::BTreeMap<String, PlannedWindow> =
            crate::collect::read_jsonl::<serde_json::Value>(&plan)
                .with_context(|| plan.display().to_string())?
                .into_iter()
                .filter_map(|v| serde_json::from_value::<PlannedWindow>(v["window"].clone()).ok())
                .map(|w| (w.window_id.clone(), w))
                .collect();
        for wid in wids {
            let Some(w) = planned.get(&wid) else {
                anyhow::bail!(
                    "window {wid} has a ledger reservation but is not in {}; the allocation cannot be reconstructed",
                    plan.display()
                );
            };
            *state.per_validator.entry(w.identity.clone()).or_default() += 1;
            *state
                .arm_counts
                .entry(w.identity.clone())
                .or_default()
                .entry(w.design.arm)
                .or_default() += 1;
            state.considered_through = state.considered_through.max(Some(w.first_slot));
        }
    }
    Ok(state)
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
            prior: init.prior,
            targets: init.targets,
            feed: init.feed,
            pool: init.pool,
            shared_sink: init.shared_sink,
            mode: init.mode,
            watches: vec![],
            feed_stale: None,
            clock_errs: vec![],
            clock_resolved: 0,
            n_windows: 0,
            last_poll: Instant::now(),
            stop: None,
            watchdog: None,
            last_hourly: Instant::now(),
            slots_dumped_through: 0,
            epoch_map: None,
            schedules_saved: std::collections::BTreeSet::new(),
            released: vec![],
            interrupt: init.interrupt.clone(),
            simulated_arms: Default::default(),
            data_root: init.data_root,
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

    fn interrupted(&self) -> bool {
        self.interrupt.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// `None` if interrupted while warming up.
    async fn wait_for_clock(&self) -> Result<Option<Fit>> {
        let end = Instant::now() + Duration::from_secs(120);
        loop {
            if self.interrupted() {
                return Ok(None);
            }
            if let Some(f) = self.fit() {
                return Ok(Some(f));
            }
            anyhow::ensure!(
                Instant::now() < end,
                "slot clock has fewer than {} observations after 120 s (slot feed down?)",
                self.cfg.min_clock_obs
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    fn write_run_json(&self) -> Result<()> {
        self.log(
            "run.json",
            &serde_json::json!({
                "run_id": self.cfg.run_id, "mode": self.mode.name(), "cluster": self.cluster.name(),
                "exploratory": !matches!(self.mode, Mode::Live { purpose: Purpose::Measure, .. }),
                "run_config": self.cfg, "plan_config": self.plan_cfg, "targets": self.targets,
                "payers": self.pool.len(), "shared_sink": self.shared_sink.map(|p| p.to_string()),
                "started": self.summary.started, "host": self.cfg.host,
                "purpose": match &self.mode { Mode::Live { purpose, .. } => Some(*purpose), Mode::DryRun { .. } => None },
            }),
        )
    }

    /// Execute pre-built windows in order (localnet tests, where one validator leads every
    /// slot and the planner finds no window boundaries). Same execution path as `run`.
    pub async fn run_fixed(mut self, windows: Vec<PlannedWindow>) -> Result<RunSummary> {
        self.write_run_json()?;
        if self.wait_for_clock().await?.is_none() {
            self.stop = Some(INTERRUPTED.into());
            return self.finish().await;
        }
        self.summary.windows_planned = windows.len();
        for w in windows {
            if self.stop.is_some() {
                break;
            }
            self.execute(w).await?;
        }
        self.finish().await
    }

    pub async fn run(mut self) -> Result<RunSummary> {
        self.write_run_json()?;
        self.watchdog = Some(crate::gaps::spawn_watchdog(
            self.run_dir.clone(),
            self.feed.clone(),
            self.cfg.clock,
        ));
        if self.wait_for_clock().await?.is_none() {
            self.stop = Some(INTERRUPTED.into());
            return self.finish().await;
        }
        let deadline = unix_ns() + i128::from(self.cfg.duration_secs) * 1_000_000_000;
        let mut state = self.prior.clone();
        let mut next_start = self.slot_resilient(deadline).await? + 10;
        let mut queue: VecDeque<PlannedWindow> = VecDeque::new();
        let mut planning_done = false;
        while self.stop.is_none() {
            if self.interrupted() {
                self.stop = Some(INTERRUPTED.into());
                break;
            }
            if unix_ns() > deadline + 60_000_000_000 {
                break; // a host stall can carry us far past the end
            }
            self.hourly_checkpoint();
            while !planning_done {
                let Some(fit) = self.fit() else {
                    // Cleared after a host stall (gaps.rs): re-warm before planning.
                    tracing::info!("slot clock re-warming");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    if unix_ns() > deadline {
                        planning_done = true;
                    }
                    break;
                };
                let t = fit.predict_ns(next_start) as i128;
                if t > deadline {
                    planning_done = true;
                    break;
                }
                if t > unix_ns() + i128::from(self.cfg.plan_ahead_secs) * 1_000_000_000 {
                    break;
                }
                let leaders = match self
                    .rpc
                    .slot_leaders(next_start, self.cfg.chunk_slots)
                    .await
                {
                    Ok(l) => l,
                    Err(e) => {
                        self.rpc_gap("getSlotLeaders", &e).await;
                        break;
                    }
                };
                self.snapshot_schedules(
                    next_start,
                    next_start + self.cfg.chunk_slots.saturating_sub(1),
                )
                .await;
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
                // Fell behind (slow RPC, host stall)? Never plan into the past.
                let now_slot = self.slot_resilient(deadline).await?;
                next_start = next_start.max(now_slot + 10);
            }
            match queue.pop_front() {
                Some(w) => {
                    let id = w.window_id.clone();
                    if let Err(e) = self.execute(w).await {
                        // Live: anything that escapes execute (ledger, disk) is fatal.
                        // Dry-run: log it and keep going (24 h unattended runs).
                        if self.mode.is_live() {
                            return Err(e);
                        }
                        tracing::warn!(window = %id, error = %format!("{e:#}"), "window failed");
                        *self
                            .summary
                            .skips
                            .entry("execute error".into())
                            .or_default() += 1;
                    }
                    for w in self.released.drain(..) {
                        planner::release(&mut state, &w);
                    }
                }
                None if planning_done => break,
                None => {
                    // nothing scheduled: a collector pass may use up to ~8 s
                    let now = unix_ns();
                    self.idle_until(now + 1_000_000_000, now + 10_000_000_000)
                        .await?
                }
            }
        }
        self.finish().await
    }

    /// getSlot that survives outages: retries every 10 s (each failure logged as an
    /// `rpc_error` gap) until it succeeds or the run is over.
    async fn slot_resilient(&mut self, deadline: i128) -> Result<u64> {
        loop {
            match self.rpc.slot().await {
                Ok(s) => return Ok(s),
                Err(e) if unix_ns() < deadline => self.rpc_gap("getSlot", &e).await,
                Err(e) => return Err(e),
            }
        }
    }

    /// Save the leader schedule of every epoch touched by [from, to] once (leaders.rs). A
    /// failure is logged as an `rpc_error` gap and retried with the next planning chunk.
    async fn snapshot_schedules(&mut self, from: u64, to: u64) {
        let start = unix_ns();
        let res: Result<()> = async {
            if self.epoch_map.is_none() {
                self.epoch_map = Some(crate::leaders::EpochMap::from_info(
                    &self.rpc.epoch_info().await?,
                ));
            }
            let m = self.epoch_map.expect("set above");
            for slot in [from, to] {
                let (epoch, _) = m.epoch_of(slot);
                if self.schedules_saved.contains(&epoch) {
                    continue;
                }
                let (epoch, written) = crate::leaders::ensure(
                    &self.rpc,
                    &self.data_root,
                    &self.cluster.name(),
                    &m,
                    slot,
                )
                .await?;
                if written {
                    tracing::info!(epoch, "leader schedule snapshot saved");
                }
                self.schedules_saved.insert(epoch);
            }
            Ok(())
        }
        .await;
        if let Err(e) = res {
            crate::gaps::log_gap(
                &self.run_dir,
                &crate::gaps::Gap::new(
                    "rpc_error",
                    start,
                    unix_ns(),
                    Some(format!("leader schedule snapshot: {e:#}")),
                ),
            );
        }
    }

    async fn rpc_gap(&mut self, method: &str, e: &anyhow::Error) {
        let start = unix_ns();
        tokio::time::sleep(Duration::from_secs(10)).await;
        crate::gaps::log_gap(
            &self.run_dir,
            &crate::gaps::Gap::new(
                "rpc_error",
                start,
                unix_ns(),
                Some(format!("{method}: {e:#}")),
            ),
        );
    }

    /// Every 30 min: append new raw slot notifications (the in-memory feed keeps ~70 min)
    /// and rewrite clock_hourly.json, so a run killed mid-way still leaves both behind.
    fn hourly_checkpoint(&mut self) {
        if self.last_hourly.elapsed() < Duration::from_secs(1_800) {
            return;
        }
        self.last_hourly = Instant::now();
        if let Err(e) = self.check_clock().and_then(|_| self.dump_slot_feed()) {
            tracing::warn!(error = %format!("{e:#}"), "hourly checkpoint failed");
        }
        self.write_hourly();
    }

    fn write_hourly(&self) {
        let r = crate::gaps::hourly(std::slice::from_ref(&self.run_dir)).and_then(|v| {
            std::fs::write(
                self.run_dir.join("clock_hourly.json"),
                serde_json::to_vec_pretty(&v)?,
            )
            .map_err(Into::into)
        });
        if let Err(e) = r {
            tracing::warn!(error = %format!("{e:#}"), "could not write clock_hourly.json");
        }
    }

    /// Drain the collector (live), let the last clock watches resolve, write summary.json.
    async fn finish(mut self) -> Result<RunSummary> {
        if let Some(w) = self.watchdog.take() {
            w.abort();
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
        self.dump_slot_feed()?;
        if let Some(ep) = self.feed_stale.take() {
            self.session_event(
                "feed_stale_at_end",
                serde_json::json!({"since": unix_ns_utc(ep.since_unix_ns),
                    "skipped_windows": ep.skipped, "run_id": self.cfg.run_id}),
            );
        }
        self.summary.unresolved = self.collector.pending_len();
        self.summary.clock_prep = clock_stats(&self.clock_errs, self.clock_resolved);
        self.summary.stopped_early = self.stop.clone();
        if let Mode::Live { ledger, .. } = &self.mode {
            let t = ledger.totals();
            self.summary.spent_run_lamports = Some(t.spent_run);
            self.summary.reserved_run_lamports = Some(t.reserved_run);
            self.summary.spent_total_lamports = Some(t.spent_total);
        }
        self.summary.credits = Some(self.rpc.meter().usage());
        self.summary.month_to_date_credits = Some(self.rpc.meter().month_to_date());
        self.summary.finished = ts();
        std::fs::write(
            self.run_dir.join("summary.json"),
            serde_json::to_vec_pretty(&self.summary)?,
        )?;
        self.write_hourly();
        Ok(self.summary)
    }

    /// Wait until `t`, doing background work. `hard` is the moment real work must start;
    /// a collector pass runs only if it can finish (bounded) 2 s before `hard`.
    async fn idle_until(&mut self, t: i128, hard: i128) -> Result<()> {
        loop {
            let now = unix_ns();
            let rem_ms = ((t - now) / 1_000_000) as i64;
            if rem_ms <= 0 {
                return Ok(());
            }
            self.check_clock()?;
            let slack_ms = ((hard - now) / 1_000_000) as i64;
            if slack_ms > 3_000
                && self.collector.pending_len() > 0
                && self.last_poll.elapsed() > Duration::from_secs(3)
            {
                self.poll_collector(Duration::from_millis((slack_ms - 2_000) as u64))
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
            let ps = self
                .summary
                .by_path
                .entry(l.tx.send.path.clone())
                .or_default();
            match l.outcome.status {
                Status::Landed => {
                    self.summary.landed += 1;
                    ps.landed += 1;
                }
                Status::Expired => {
                    self.summary.expired += 1;
                    ps.expired += 1;
                }
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
        self.clock_resolved += lines.len();
        for l in &lines {
            if let Some(e) = l.err_prep_ms {
                self.clock_errs.push(e);
            }
        }
        collect::append_jsonl(&self.run_dir.join("clock.jsonl"), &lines)
    }

    /// Raw slot notifications (slot, first-seen UNIX ns) for the clock-model error report.
    fn dump_slot_feed(&mut self) -> Result<()> {
        let s = self
            .feed
            .lock()
            .map_err(|_| anyhow::anyhow!("slot state poisoned"))?;
        let rows: Vec<serde_json::Value> = s
            .first_seen
            .range(self.slots_dumped_through + 1..)
            .map(|(k, v)| serde_json::json!({"slot": k, "t_ns": v}))
            .collect();
        let last = s.first_seen.last_key_value().map(|(k, _)| *k);
        drop(s);
        if let Some(l) = last {
            self.slots_dumped_through = self.slots_dumped_through.max(l);
        }
        collect::append_jsonl(&self.run_dir.join("slots.jsonl"), &rows)
    }

    /// Staleness gate: `Some(reason)` if the WebSocket slot feed is stale, i.e. the clock is
    /// extrapolating. Records the age on the window log and writes one `feed_stale` session
    /// event per episode, then `feed_recovered` (with the number of skipped windows) once a
    /// later window passes. Dry-run and live behave the same.
    fn feed_gate(&mut self, log: &mut WindowLog) -> Option<String> {
        let now = unix_ns();
        let (reason, age) = match self.feed.lock() {
            Ok(s) => (s.stale_reason(now), s.ws_age_ms(now)),
            Err(_) => (Some("slot feed stale: slot state poisoned".into()), None),
        };
        log.feed_age_ms = age;
        match (&reason, self.feed_stale.as_mut()) {
            (Some(r), None) => {
                tracing::warn!(window = %log.window.window_id, reason = %r, "slot feed stale: not sending");
                self.session_event(
                    "feed_stale",
                    serde_json::json!({"window_id": log.window.window_id, "reason": r,
                        "ws_age_ms": age, "run_id": self.cfg.run_id}),
                );
                self.feed_stale = Some(StaleEpisode {
                    since_unix_ns: now,
                    skipped: 1,
                });
            }
            (Some(_), Some(ep)) => ep.skipped += 1,
            (None, Some(_)) => {
                let ep = self.feed_stale.take().expect("matched Some");
                tracing::info!(skipped = ep.skipped, "slot feed fresh again");
                self.session_event(
                    "feed_recovered",
                    serde_json::json!({"window_id": log.window.window_id,
                        "since": unix_ns_utc(ep.since_unix_ns), "skipped_windows": ep.skipped,
                        "ws_age_ms": age, "run_id": self.cfg.run_id}),
                );
            }
            (None, None) => {}
        }
        reason
    }

    /// Append to the session's events.jsonl (only under `sender session`). Never fatal.
    fn session_event(&self, kind: &str, fields: serde_json::Value) {
        let Some(path) = &self.cfg.session_events else {
            return;
        };
        let now = (unix_ns() / 1_000_000_000) as i64;
        if let Err(e) = crate::session::append_event(path, now, kind, fields) {
            tracing::warn!(error = %format!("{e:#}"), kind, "could not write session event");
        }
    }

    fn skip(&mut self, mut log: WindowLog, reason: String) -> Result<()> {
        tracing::info!(window = %log.window.window_id, %reason, "window skipped");
        *self.summary.skips.entry(reason_key(&reason)).or_default() += 1;
        if log.reserved_lamports == 0 {
            // never attempted: free its quota (a reserved window stays counted, as in the ledger)
            self.released.push(log.window.clone());
        }
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

    async fn execute(&mut self, mut w: PlannedWindow) -> Result<()> {
        let k = self.n_windows;
        self.n_windows += 1;
        let path = match &self.mode {
            Mode::Live { .. } => Some(match self.cfg.rpc_every {
                Some(e) if e > 0 && k % e == e - 1 => PathKind::Rpc,
                _ => PathKind::Quic,
            }),
            Mode::DryRun { .. } => None,
        };
        if path == Some(PathKind::Rpc) {
            // Delivery check, never a burst arm: exactly one tx (the first in send order).
            // The provider allows 1 sendTransaction/s, so a burst would spread over seconds.
            // Its txs are marked `ordering_eligible: false` in the manifest.
            w.design.txs.retain(|t| t.send_rank == 0);
            w.design.txs.truncate(1);
            let lps = self.plan_cfg.lamports_per_signature;
            w.expected_cost_lamports = w
                .design
                .txs
                .iter()
                .map(|t| total_fee(lps, 1, t.cu_price_micro_lamports, t.cu_limit))
                .sum();
        }
        let base_offset_ms = if self.cfg.offsets_ms.is_empty() {
            self.plan_cfg.offset_ms
        } else {
            self.cfg.offsets_ms[k % self.cfg.offsets_ms.len()]
        };
        // Arm F's gapped burst must be held whole before the window starts (arms.rs).
        let offset_ms = base_offset_ms - w.design.extra_lead_ms() as i64;
        let offset_ns = i128::from(offset_ms) * 1_000_000;
        let prep_ns = i128::from(self.cfg.prep_ms) * 1_000_000;
        // Plan-time predictions can be minutes ahead and off by seconds (LOGBOOK), so the
        // wait re-predicts at least once a second and prep starts prep_ms before the
        // *current* prediction.
        loop {
            if self.interrupted() {
                // not started: nothing reserved, nothing sent; its quota is freed
                self.released.push(w);
                return Ok(());
            }
            let pred = self
                .fit()
                .map(|f| f.predict_ns(w.first_slot) as i128)
                .unwrap_or(w.predicted_first_slot_unix_ns);
            let prep_at = pred + offset_ns - prep_ns;
            let now = unix_ns();
            if now >= prep_at {
                break;
            }
            self.idle_until(prep_at.min(now + 1_000_000_000), prep_at)
                .await?;
        }

        let fit = self.fit();
        let predicted_prep = fit.map(|f| f.predict_ns(w.first_slot) as i128);
        let send_at = predicted_prep.unwrap_or(w.predicted_first_slot_unix_ns) + offset_ns;
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
            feed_age_ms: None,
        };
        // Nothing is prepared on a stale clock: the re-prediction above extrapolated.
        if let Some(reason) = self.feed_gate(&mut log) {
            return self.skip(log, reason);
        }
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

        let first_of_arm = !self.simulated_arms.contains(&d.arm);
        if self.cfg.simulate_every > 0
            && (first_of_arm || k.is_multiple_of(self.cfg.simulate_every))
        {
            // The binding tx: the smallest CU limit (Arm R spans 1,000-6,000), ties -> costliest.
            let ksim = (0..d.txs.len())
                .min_by_key(|&i| (d.txs[i].cu_limit, std::cmp::Reverse(costs[i])))
                .unwrap_or(kmax);
            let limit = d.txs[ksim].cu_limit;
            let sim = self.rpc.simulate(&b64(&built[ksim].wire)).await;
            let units = sim.as_ref().ok().and_then(|v| v["unitsConsumed"].as_u64());
            let loaded = sim
                .as_ref()
                .ok()
                .and_then(|v| v["loadedAccountsDataSize"].as_u64());
            log.simulation = Some(match &sim {
                Ok(v) => serde_json::json!({"err": v["err"], "idx": d.txs[ksim].idx,
                    "units_consumed": units, "cu_limit": limit,
                    "loaded_accounts_data_size": loaded,
                    "loaded_accounts_data_limit": crate::txbuild::LOADED_ACCOUNTS_DATA_LIMIT}),
                Err(e) => serde_json::json!({"rpc_error": format!("{e:#}")}),
            });
            let stats = self
                .summary
                .simulation_by_arm
                .entry(format!("{:?}", d.arm))
                .or_default();
            match &sim {
                Ok(v) => stats.record(limit, units, loaded, &v["err"]),
                Err(_) => stats.record(limit, None, None, &serde_json::json!("rpc_error")),
            }
            self.simulated_arms.insert(d.arm);
            if self.mode.is_live()
                && let Ok(v) = &sim
                && v["err"].is_null()
            {
                let over_cu = units.is_some_and(|u| u >= u64::from(limit));
                let over_loaded = loaded
                    .is_some_and(|l| l >= u64::from(crate::txbuild::LOADED_ACCOUNTS_DATA_LIMIT));
                if over_cu || over_loaded {
                    self.stop = Some(format!(
                        "limits too low: CU {units:?} vs limit {limit}, loaded data {loaded:?} vs limit {}",
                        crate::txbuild::LOADED_ACCOUNTS_DATA_LIMIT
                    ));
                    return self.skip(log, "CU or loaded-data limit below usage".into());
                }
            }
        }

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
                rank: t.send_rank,
            })
            .collect();

        // The feed may have died during prep: check again before anything is reserved or sent.
        if let Some(reason) = self.feed_gate(&mut log) {
            if let Some(c) = &conn {
                c.close();
            }
            return self.skip(log, reason);
        }

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
                        let mut sink = RpcSink::new(&self.rpc, auth, self.cfg.rpc_path_label);
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
                    let ps = self.summary.by_path.entry(rec.path.clone()).or_default();
                    ps.sent += 1;
                    if rec.error.is_some() {
                        self.summary.send_errors += 1;
                        ps.send_errors += 1;
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
                        tpu_quic: w.tpu_quic.clone(),
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
                        ordering_eligible: path == Some(PathKind::Quic),
                        host: self.cfg.host.clone(),
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
            send_rank: t.rank,
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
    /// RFC 3339 with `Z`, never an offset (analysis/tests/test_utc.py checks the data side).
    fn assert_utc_z(s: &str) {
        assert!(s.ends_with('Z'), "{s}");
        let t = chrono::DateTime::parse_from_rfc3339(s).unwrap();
        assert_eq!(t.offset().local_minus_utc(), 0, "{s}");
    }

    #[test]
    fn timestamps_are_utc_z() {
        assert_utc_z(&ts());
    }

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
    fn simulation_stats_per_arm() {
        let mut s = SimArmStats::default();
        s.record(
            1_000,
            Some(0),
            None,
            &serde_json::json!({"InstructionError": [0, "x"]}),
        );
        s.record(1_000, Some(0), None, &serde_json::json!("AccountNotFound"));
        assert_eq!((s.simulated, s.ok), (2, 0));
        assert_eq!(s.errors.get("AccountNotFound"), Some(&1));
        assert_eq!(s.errors.get("InstructionError"), Some(&1));
        assert_eq!(s.within_limits, None, "no ok simulation yet");
        s.record(1_000, Some(450), Some(2_000), &serde_json::Value::Null);
        s.record(6_000, Some(470), Some(2_100), &serde_json::Value::Null);
        assert_eq!(
            (s.units_min, s.units_max, s.cu_limit_min),
            (Some(450), Some(470), Some(1_000))
        );
        assert_eq!(
            (s.loaded_bytes_max, s.loaded_bytes_limit),
            (Some(2_100), 32 * 1024)
        );
        assert_eq!(s.within_limits, Some(true));
        s.record(1_000, Some(1_000), Some(2_000), &serde_json::Value::Null);
        assert_eq!(s.within_limits, Some(false), "usage == limit does not fit");
    }

    /// Ctrl+C (the interrupt flag) during a dry-run: the run stops and still writes
    /// summary.json, with `stopped_early` saying why. Offline: nothing touches the network.
    #[tokio::test]
    async fn interrupt_writes_summary_json() {
        let dir = tempfile::tempdir().unwrap();
        let interrupt: std::sync::Arc<std::sync::atomic::AtomicBool> = Default::default();
        let runner = Runner::new(RunnerInit {
            rpc: Rpc::new("http://127.0.0.1:9"),
            cluster: Cluster::Mainnet,
            data_root: dir.path().to_path_buf(),
            cfg: RunConfig {
                run_id: "dryrun-20261023T150000Z".into(),
                ..RunConfig::default()
            },
            plan_cfg: PlanConfig::default(),
            targets: vec![],
            feed: crate::feed::new_state(),
            pool: PayerPool::new(vec![], HashSet::new(), None, 0),
            shared_sink: None,
            mode: Mode::DryRun {
                probe_connect: false,
            },
            prior: PlanState::default(),
            interrupt: interrupt.clone(),
        })
        .unwrap();
        interrupt.store(true, std::sync::atomic::Ordering::SeqCst);
        let t0 = Instant::now();
        let summary = runner.run().await.unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "must not wait for the slot clock"
        );
        assert_eq!(summary.stopped_early.as_deref(), Some(INTERRUPTED));
        let run_dir = dir.path().join("logs").join("dryrun-20261023T150000Z");
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(run_dir.join("summary.json")).unwrap()).unwrap();
        assert_eq!(written["stopped_early"], INTERRUPTED);
        assert!(written["finished"].as_str().unwrap().ends_with('Z'));
        assert!(run_dir.join("run.json").exists());
    }

    /// Feed stall: with the last WebSocket notification 3 s old, a window is skipped before
    /// anything is prepared (no RPC call, no payer, no send) and one `feed_stale` session event
    /// is written; once notifications flow again the next window passes the gate and a
    /// `feed_recovered` event counts the skipped windows. Offline.
    #[tokio::test]
    async fn stale_slot_feed_blocks_windows_and_logs_session_events() {
        const MS: i128 = 1_000_000;
        const SLOT: i128 = 400 * MS;
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join("events.jsonl");
        let feed = crate::feed::new_state();
        let mut runner = Runner::new(RunnerInit {
            rpc: Rpc::new("http://127.0.0.1:9"),
            cluster: Cluster::Mainnet,
            data_root: dir.path().to_path_buf(),
            cfg: RunConfig {
                run_id: "dryrun-20261024T120000Z".into(),
                session_events: Some(events.clone()),
                ..RunConfig::default()
            },
            plan_cfg: PlanConfig::default(),
            targets: vec![],
            feed: feed.clone(),
            pool: PayerPool::new(vec![], HashSet::new(), None, 0),
            shared_sink: None,
            mode: Mode::DryRun {
                probe_connect: false,
            },
            prior: PlanState::default(),
            interrupt: Default::default(),
        })
        .unwrap();
        // 150 notifications, one per 400 ms, the newest 3 s ago; then silence.
        let now = unix_ns();
        let last_t = now - 3_000 * MS;
        for k in 0..150u64 {
            crate::feed::record(&feed, 1_000 + k, last_t - (149 - k as i128) * SLOT, "ws");
        }
        let window = |first_slot: u64, at: i128| PlannedWindow {
            window_id: format!("{first_slot}-test"),
            identity: "id".into(),
            vote_account: "vote".into(),
            stratum: "Agave".into(),
            tpu_quic: None,
            first_slot,
            n_slots: 4,
            epoch: 1066,
            predicted_first_slot_unix_ns: at,
            send_at_unix_ns: at - 100 * MS,
            lead_ms: 5_000,
            design: crate::arms::design(&PlanConfig::default().arm_cfg, Arm::P, 0, 7, None),
            expected_cost_lamports: 0,
        };
        // Predicted ~1.8 s ahead: prep is due at once, and the stale gate must fire.
        let slot = 1_149 + 12;
        runner
            .execute(window(slot, last_t + 12 * SLOT))
            .await
            .unwrap();
        // Still stale: counted in the same episode, no second event.
        runner
            .execute(window(slot + 1, last_t + 13 * SLOT))
            .await
            .unwrap();
        // Fresh notifications up to now, then a window ~200 ms ahead: it passes the gate and is
        // skipped as late (offline: no RPC needed).
        let mut s = 1_150u64;
        let mut t = last_t + SLOT;
        while t <= unix_ns() {
            crate::feed::record(&feed, s, t, "ws");
            s += 1;
            t += SLOT;
        }
        let at = unix_ns() + 200 * MS;
        let s_late = (s - 1) + ((at - (t - SLOT)) / SLOT) as u64;
        runner.execute(window(s_late, at)).await.unwrap();

        let run_dir = dir.path().join("logs").join("dryrun-20261024T120000Z");
        let logs: Vec<serde_json::Value> =
            collect::read_jsonl(&run_dir.join("windows.jsonl")).unwrap();
        assert_eq!(logs.len(), 3);
        for l in &logs[..2] {
            assert_eq!(l["outcome"], "skipped");
            let r = l["skip_reason"].as_str().unwrap();
            assert!(
                r.starts_with("slot feed stale: last WebSocket notification"),
                "{r}"
            );
            assert!(l["feed_age_ms"].as_i64().unwrap() > 2_000);
            assert!(l["blockhash"].is_null() && l["sends"].as_array().unwrap().is_empty());
        }
        let r = logs[2]["skip_reason"].as_str().unwrap();
        assert!(r.starts_with("late after re-prediction"), "{r}");
        assert!(logs[2]["feed_age_ms"].as_i64().unwrap() <= 2_000);
        let ev: Vec<serde_json::Value> = collect::read_jsonl(&events).unwrap();
        let kinds: Vec<&str> = ev.iter().map(|e| e["event"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["feed_stale", "feed_recovered"]);
        assert_eq!(ev[0]["window_id"], format!("{slot}-test"));
        assert_eq!(ev[1]["skipped_windows"], 2);
        assert!(ev.iter().all(|e| e["ts"].as_str().unwrap().ends_with('Z')));
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
