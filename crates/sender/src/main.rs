//! `sender`: planner, clock model, tx builder, QUIC sender and spend ledger.
//!
//! Dry-run is the default everywhere. Only `live` and `fund/sweep --live` can move lamports,
//! and each needs `CONFIRM_LIVE=yes` plus an explicit `--max-spend-lamports` that cannot exceed
//! the compile-time ceiling (see `live::authorize`).

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use slwlab_common::guard;
use slwlab_common::limits::{HARD_CEILING_LAMPORTS, PLANNED_SPEND_BUDGET_LAMPORTS};
use slwlab_sender::arms::{Arm, ArmConfig};
use slwlab_sender::burst::unix_ns;
use slwlab_sender::cluster::Cluster;
use slwlab_sender::collect::{self, Collector, Pending, SentTx};
use slwlab_sender::funding::{self, Role};
use slwlab_sender::keys::{self, NamedKeypair};
use slwlab_sender::ledger::{Entry, Ledger, Purpose};
use slwlab_sender::live::{self, LiveAuth};
use slwlab_sender::planner::{self, PlanConfig, PlanState, Target};
use slwlab_sender::rpc::Rpc;
use slwlab_sender::runner::{self, Mode, PayerPool, RunConfig, Runner, RunnerInit};
use slwlab_sender::select;
use slwlab_sender::{feed, rpc};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "sender",
    version,
    about = "Leader-window ordering experiment (dry-run by default)"
)]
struct Cli {
    #[command(flatten)]
    g: Global,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct Global {
    #[arg(
        long,
        env = "RPC_URL",
        default_value = "https://api.mainnet-beta.solana.com",
        global = true
    )]
    rpc_url: String,
    /// Slot feed WebSocket (default: derived from --rpc-url).
    #[arg(long, env = "WS_URL", global = true)]
    ws_url: Option<String>,
    #[arg(long, default_value = "data", global = true)]
    data_dir: PathBuf,
    #[arg(long, default_value = "secrets", global = true)]
    secrets_dir: PathBuf,
    #[arg(
        long,
        env = "KOBE_URL",
        default_value = "https://kobe.mainnet.jito.network/api/v1/validators",
        global = true
    )]
    kobe_url: String,
}

#[derive(Args, Clone)]
struct PlanArgs {
    /// Selection snapshot (default: newest data/snapshots/selection_*.json).
    #[arg(long)]
    selection: Option<PathBuf>,
    /// Campaign seed for arm assignment and within-arm randomization (recorded).
    #[arg(long, default_value_t = 20_261_022)]
    seed: u64,
    #[arg(long, default_value_t = -100, allow_hyphen_values = true)]
    offset_ms: i64,
    #[arg(long, default_value_t = 8)]
    n: usize,
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "P,F,X,R,C",
        ignore_case = true
    )]
    arms: Vec<ArmArg>,
    #[arg(long, default_value_t = guard::DEFAULT_MIN_LIVE_EPOCH)]
    min_live_epoch: u64,
    #[arg(long, default_value_t = 5_000)]
    min_spacing_ms: u64,
    #[arg(long)]
    max_windows: Option<usize>,
    /// Payer pool size for dry-runs without ./secrets/payers.
    #[arg(long, default_value_t = 32)]
    ephemeral_payers: usize,
}

#[derive(Args, Clone)]
struct RunArgs {
    #[command(flatten)]
    plan: PlanArgs,
    #[arg(long, default_value_t = 600)]
    duration_secs: u64,
    /// Calibration offset sweep, e.g. -300,-100,0,100 (cycled per window).
    #[arg(long, value_delimiter = ',', allow_hyphen_values = true)]
    offsets: Vec<i64>,
    #[arg(long, default_value_t = 25)]
    simulate_every: usize,
    #[arg(long, default_value_t = 1_500)]
    prep_ms: u64,
}

#[derive(Clone, Copy, ValueEnum)]
enum ArmArg {
    P,
    F,
    X,
    R,
    C,
}

impl From<ArmArg> for Arm {
    fn from(a: ArmArg) -> Arm {
        match a {
            ArmArg::P => Arm::P,
            ArmArg::F => Arm::F,
            ArmArg::X => Arm::X,
            ArmArg::R => Arm::R,
            ArmArg::C => Arm::C,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum PurposeArg {
    Measure,
    Calibrate,
}

#[derive(Subcommand)]
enum Cmd {
    /// Snapshot cluster/vote/Kobe/leader-schedule data and pick validators per stratum.
    SelectValidators {
        #[arg(long, default_value_t = 40)]
        min_windows: usize,
    },
    /// Print the window plan for the next --slots slots (no waiting, no sending).
    Plan {
        #[command(flatten)]
        plan: PlanArgs,
        #[arg(long, default_value_t = 3_000)]
        slots: u64,
    },
    /// Run the campaign loop in real time without sending anything.
    DryRun {
        #[command(flatten)]
        run: RunArgs,
        /// Open QUIC connections to measure handshake/RTT (never opens a stream).
        #[arg(long)]
        probe_connect: bool,
    },
    /// Send real transactions. Requires CONFIRM_LIVE=yes and --max-spend-lamports.
    Live {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long)]
        max_spend_lamports: Option<u64>,
        #[arg(long, value_enum)]
        purpose: PurposeArg,
        /// Commit of the `prereg-v1` tag; required for --purpose measure.
        #[arg(long)]
        prereg: Option<String>,
        /// Every k-th window goes through RPC sendTransaction (path comparison).
        #[arg(long)]
        rpc_every: Option<usize>,
    },
    /// Resolve every open reservation and every sent tx missing from the manifests.
    Collect,
    /// Create payer keypairs (and the Arm C sink) under ./secrets. Prints pubkeys only.
    InitPayers {
        #[arg(long, default_value_t = 32)]
        count: usize,
    },
    /// Save a balance snapshot of master, payers and sink.
    Balances,
    /// Top up payers and the sink from the master (dry-run unless --live).
    Fund {
        #[arg(long)]
        payer_target_lamports: u64,
        #[arg(long)]
        live: bool,
        #[arg(long)]
        max_spend_lamports: Option<u64>,
    },
    /// Return payer and sink balances to the master (dry-run unless --live).
    Sweep {
        #[arg(long)]
        live: bool,
        #[arg(long)]
        max_spend_lamports: Option<u64>,
    },
    /// Compare wallet balances, ledger and on-chain fees between two snapshots.
    Reconcile {
        #[arg(long)]
        since: PathBuf,
        /// Closing snapshot (default: take a new one now).
        #[arg(long)]
        until: Option<PathBuf>,
    },
}

struct Ctx {
    g: Global,
    rpc: Rpc,
    cluster: Cluster,
    data_root: PathBuf,
}

impl Ctx {
    fn ledger_path(&self) -> PathBuf {
        self.data_root.join("ledger.jsonl")
    }
    fn payers_dir(&self) -> PathBuf {
        self.g.secrets_dir.join("payers")
    }
    fn sink_path(&self) -> PathBuf {
        self.g.secrets_dir.join("sink.json")
    }
    fn master_path(&self) -> PathBuf {
        std::env::var("MASTER_KEYPAIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| self.g.secrets_dir.join("master.json"))
    }
}

fn stamp() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

fn init_tracing(log_file: Option<&Path>) -> Result<()> {
    use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};
    let filter = || EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr = fmt::layer()
        .with_writer(std::io::stderr)
        .compact()
        .with_filter(filter());
    let file = match log_file {
        Some(p) => {
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)?;
            Some(
                fmt::layer()
                    .json()
                    .with_writer(std::sync::Mutex::new(f))
                    .with_filter(filter()),
            )
        }
        None => None,
    };
    use tracing_subscriber::Layer;
    tracing_subscriber::registry()
        .with(stderr)
        .with(file)
        .try_init()?;
    Ok(())
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = real_main(cli).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn real_main(cli: Cli) -> Result<()> {
    rpc::init_crypto();
    let rpc = Rpc::new(&cli.g.rpc_url);
    let genesis = rpc.genesis_hash().await.context("getGenesisHash")?;
    let cluster = Cluster::from_genesis(&genesis);
    let ctx = Ctx {
        data_root: cluster.data_root(&cli.g.data_dir),
        g: cli.g,
        rpc,
        cluster,
    };
    let cmd = cli.cmd;
    let (prefix, is_run) = match &cmd {
        Cmd::DryRun { .. } => ("dryrun", true),
        Cmd::Live { .. } => ("live", true),
        Cmd::Fund { .. } => ("fund", false),
        Cmd::Sweep { .. } => ("sweep", false),
        _ => ("cmd", false),
    };
    let run_id = format!("{prefix}-{}", stamp());
    let log_file = is_run.then(|| {
        ctx.data_root
            .join("logs")
            .join(&run_id)
            .join("run.log.jsonl")
    });
    init_tracing(log_file.as_deref())?;
    tracing::info!(cluster = %ctx.cluster.name(), data_root = %ctx.data_root.display(), "connected");
    match cmd {
        Cmd::SelectValidators { min_windows } => select_validators(&ctx, min_windows).await,
        Cmd::Plan { plan, slots } => plan_only(&ctx, &plan, slots).await,
        Cmd::DryRun { run, probe_connect } => {
            campaign(
                &ctx,
                &run,
                &run_id,
                None,
                Mode::DryRun { probe_connect },
                None,
                None,
            )
            .await
        }
        Cmd::Live {
            run,
            max_spend_lamports,
            purpose,
            prereg,
            rpc_every,
        } => {
            let auth = live::authorize(
                true,
                std::env::var(live::CONFIRM_ENV).ok().as_deref(),
                max_spend_lamports,
            )?;
            let purpose = match purpose {
                PurposeArg::Measure => Purpose::Measure,
                PurposeArg::Calibrate => Purpose::Calibrate,
            };
            if purpose == Purpose::Measure {
                ensure!(
                    ctx.cluster.is_mainnet(),
                    "confirmatory measurements are mainnet-only"
                );
                ensure!(
                    prereg.as_deref().is_some_and(|p| p.len() >= 7),
                    "--purpose measure requires --prereg <commit of the prereg-v1 tag>"
                );
            }
            let mut ledger = Ledger::open(&ctx.ledger_path(), &run_id, auth.cap())?;
            report_open_reservations(&ledger);
            campaign(
                &ctx,
                &run,
                &run_id,
                rpc_every,
                Mode::Live {
                    auth: &auth,
                    ledger: &mut ledger,
                    purpose,
                },
                prereg,
                Some(&auth),
            )
            .await
        }
        Cmd::Collect => collect_all(&ctx).await,
        Cmd::InitPayers { count } => {
            let pks = keys::init_payers(&ctx.payers_dir(), count)?;
            for (i, p) in pks.iter().enumerate() {
                println!("payer_{i:03} {p}");
            }
            let sink = load_or_create_sink(&ctx.sink_path())?;
            println!("sink {}", sink.pubkey());
            Ok(())
        }
        Cmd::Balances => {
            let s = funding::snapshot(&ctx.rpc, &ctx.cluster.name(), &wallets(&ctx)?).await?;
            let p = funding::save_snapshot(&ctx.data_root, &s)?;
            println!(
                "{} total {} lamports, locked in payers+sink {} (slot {})",
                p.display(),
                s.total(),
                s.locked(),
                s.slot
            );
            Ok(())
        }
        Cmd::Fund {
            payer_target_lamports,
            live,
            max_spend_lamports,
        } => {
            fund(
                &ctx,
                payer_target_lamports,
                live,
                max_spend_lamports,
                &run_id,
            )
            .await
        }
        Cmd::Sweep {
            live,
            max_spend_lamports,
        } => sweep(&ctx, live, max_spend_lamports, &run_id).await,
        Cmd::Reconcile { since, until } => {
            let before = funding::load_snapshot(&since)?;
            let after = match until {
                Some(p) => funding::load_snapshot(&p)?,
                None => {
                    let s =
                        funding::snapshot(&ctx.rpc, &ctx.cluster.name(), &wallets(&ctx)?).await?;
                    funding::save_snapshot(&ctx.data_root, &s)?;
                    s
                }
            };
            let r = funding::reconcile(
                &ctx.rpc,
                &ctx.data_root,
                &ctx.ledger_path(),
                &before,
                &after,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            funding::fail_if_not_ok(&r)
        }
    }
}

fn report_open_reservations(ledger: &Ledger) {
    let open: Vec<_> = ledger.open_reservations().collect();
    if !open.is_empty() {
        tracing::warn!(
            n = open.len(),
            "unresolved reservations from earlier runs stay reserved; run `sender collect`"
        );
    }
}

fn load_or_create_sink(path: &Path) -> Result<NamedKeypair> {
    if !path.exists() {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        keys::write_new_keypair(path, &solana_keypair::Keypair::new())?;
    }
    Ok(NamedKeypair {
        name: "sink".into(),
        keypair: keys::read_keypair(path)?,
    })
}

/// Master (pubkey only unless signing), payers and sink, for snapshots.
fn wallets(ctx: &Ctx) -> Result<Vec<(Pubkey, Role)>> {
    let mut v = vec![(
        keys::read_keypair(&ctx.master_path())?.pubkey(),
        Role::Master,
    )];
    for p in keys::load_payers(&ctx.payers_dir())? {
        v.push((p.pubkey(), Role::Payer));
    }
    if ctx.sink_path().exists() {
        v.push((keys::read_keypair(&ctx.sink_path())?.pubkey(), Role::Sink));
    }
    Ok(v)
}

fn latest_selection(ctx: &Ctx, explicit: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.clone());
    }
    let dir = ctx.data_root.join("snapshots");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("{}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("selection_") && n.ends_with(".json"))
        })
        .collect();
    v.sort();
    v.pop()
        .context("no selection snapshot: run `sender select-validators` first")
}

/// Targets from a selection snapshot, with tpuQuic refreshed from gossip right now.
async fn load_targets(ctx: &Ctx, path: &Path) -> Result<Vec<Target>> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let picked: Vec<select::Candidate> = serde_json::from_value(v["picked"].clone())?;
    let nodes = ctx.rpc.cluster_nodes().await?;
    let tpu: HashMap<&str, Option<String>> = nodes
        .iter()
        .map(|n| (n.pubkey.as_str(), n.tpu_quic.clone()))
        .collect();
    Ok(picked
        .into_iter()
        .map(|c| Target {
            tpu_quic: tpu.get(c.identity.as_str()).cloned().flatten(),
            identity: c.identity,
            vote_account: c.vote_account,
            stratum: format!("{:?}", c.stratum),
        })
        .collect())
}

async fn select_validators(ctx: &Ctx, min_windows: usize) -> Result<()> {
    let ts = stamp();
    let epoch = ctx.rpc.epoch_info().await?;
    let nodes_raw = ctx.rpc.cluster_nodes_raw().await?;
    let nodes: Vec<rpc::ClusterNode> = serde_json::from_value(nodes_raw.clone())?;
    let votes_raw = ctx
        .rpc
        .call("getVoteAccounts", serde_json::json!([]))
        .await?;
    let votes: rpc::VoteAccounts = serde_json::from_value(votes_raw.clone())?;
    let (kobe_raw, kobe_note) = match fetch_json(&ctx.g.kobe_url).await {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(format!("Kobe unavailable: {e:#}"))),
    };
    let kobe = kobe_raw
        .as_ref()
        .map(select::kobe_by_identity)
        .unwrap_or_default();
    // Horizon: rest of this epoch + the next one (when its schedule is already known).
    let epoch_start = epoch.absolute_slot - epoch.slot_index;
    let mut windows = select::count_windows(
        &ctx.rpc.leader_schedule(epoch.absolute_slot).await?,
        epoch_start,
        epoch.absolute_slot,
    );
    let next_start = epoch_start + epoch.slots_in_epoch;
    let mut horizon_end = next_start;
    match ctx.rpc.leader_schedule(next_start).await {
        Ok(s) => {
            for (k, n) in select::count_windows(&s, next_start, next_start) {
                *windows.entry(k).or_default() += n;
            }
            horizon_end = next_start + epoch.slots_in_epoch;
        }
        Err(e) => tracing::warn!(error = %format!("{e:#}"), "next epoch schedule unavailable"),
    }
    let cands = select::candidates(&nodes, &votes, &kobe, &windows);
    let (picked, mut notes) = select::pick(&cands, &select::quotas(), min_windows);
    notes.extend(kobe_note);
    let dir = ctx.data_root.join("snapshots");
    std::fs::create_dir_all(&dir)?;
    for (name, v) in [
        ("cluster_nodes", Some(&nodes_raw)),
        ("vote_accounts", Some(&votes_raw)),
        ("kobe_validators", kobe_raw.as_ref()),
    ] {
        if let Some(v) = v {
            let f = std::fs::File::create(dir.join(format!("{name}_{ts}.json.gz")))?;
            let mut gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
            serde_json::to_writer(&mut gz, v)?;
            gz.finish()?;
        }
    }
    let path = dir.join(format!("selection_{ts}.json"));
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "ts": ts, "cluster": ctx.cluster.name(), "epoch": epoch.epoch,
            "slot": epoch.absolute_slot, "horizon_end_slot": horizon_end,
            "min_windows": min_windows, "quotas": select::quotas(),
            "raw": {"cluster_nodes": format!("cluster_nodes_{ts}.json.gz"),
                    "vote_accounts": format!("vote_accounts_{ts}.json.gz"),
                    "kobe_validators": kobe_raw.as_ref().map(|_| format!("kobe_validators_{ts}.json.gz"))},
            "notes": notes, "picked": picked, "candidates": cands,
        }))?,
    )?;
    println!("{}", path.display());
    println!(
        "{:<46} {:<20} {:<7} {:>8} {:>14}",
        "identity", "stratum", "conf", "windows", "stake SOL"
    );
    for c in &picked {
        println!(
            "{:<46} {:<20} {:<7} {:>8} {:>14.0}",
            c.identity,
            format!("{:?}", c.stratum),
            format!("{:?}", c.confidence),
            c.windows_in_horizon,
            c.stake_lamports as f64 / 1e9
        );
    }
    for n in &notes {
        println!("note: {n}");
    }
    Ok(())
}

async fn fetch_json(url: &str) -> Result<serde_json::Value> {
    let r = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?;
    Ok(r.json().await?)
}

async fn lamports_per_signature(rpc: &Rpc) -> Result<u64> {
    let probe = keys::ephemeral_payers(1);
    // A zero-priority transfer with one signature costs exactly lamports_per_signature.
    funding::transfer_fee(rpc, &probe[0], &Pubkey::new_unique()).await
}

fn plan_config(ctx: &Ctx, a: &PlanArgs, lps: u64, spent_reserved: u64) -> PlanConfig {
    PlanConfig {
        campaign_seed: a.seed,
        offset_ms: a.offset_ms,
        min_spacing_ms: a.min_spacing_ms,
        min_live_epoch: a.min_live_epoch,
        max_windows: a.max_windows.unwrap_or(usize::MAX),
        lamports_per_signature: lps,
        budget_lamports: PLANNED_SPEND_BUDGET_LAMPORTS.saturating_sub(spent_reserved),
        arms: a.arms.iter().map(|&x| x.into()).collect(),
        arm_cfg: ArmConfig {
            n: a.n,
            ..ArmConfig::default()
        },
        mainnet_guard: ctx.cluster.is_mainnet(),
        ..PlanConfig::default()
    }
}

/// spent + reserved over the whole project, read-only (dry-runs must not write the ledger).
fn ledger_commitment(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut open: BTreeMap<String, u64> = BTreeMap::new();
    let mut spent = 0u64;
    for e in Ledger::read_entries(path)? {
        match e {
            Entry::Reserve { sig, amount, .. } => {
                open.insert(sig, amount);
            }
            Entry::Landed { sig, fee, .. } => {
                open.remove(&sig);
                spent += fee;
            }
            Entry::Expired { sig, .. } => {
                open.remove(&sig);
            }
            _ => {}
        }
    }
    Ok(spent + open.values().sum::<u64>())
}

async fn warm_clock(ctx: &Ctx) -> Result<feed::Shared> {
    let state = feed::new_state();
    let ws = ctx
        .g
        .ws_url
        .clone()
        .unwrap_or_else(|| feed::ws_from_http(&ctx.g.rpc_url));
    feed::spawn(Some(ws), ctx.rpc.clone(), state.clone());
    Ok(state)
}

async fn plan_only(ctx: &Ctx, a: &PlanArgs, slots: u64) -> Result<()> {
    // A fresh clone (or `docker compose up`) has no selection yet: make one first.
    let sel = match latest_selection(ctx, a.selection.as_ref()) {
        Ok(p) => p,
        Err(_) if a.selection.is_none() => {
            tracing::info!("no selection snapshot yet: running select-validators first");
            select_validators(ctx, 40).await?;
            latest_selection(ctx, None)?
        }
        Err(e) => return Err(e),
    };
    let targets = load_targets(ctx, &sel).await?;
    let lps = lamports_per_signature(&ctx.rpc).await?;
    let committed = ledger_commitment(&ctx.ledger_path())?;
    let cfg = plan_config(ctx, a, lps, committed);
    let state = warm_clock(ctx).await?;
    let fit = loop {
        if let Some(f) = state
            .lock()
            .ok()
            .and_then(|s| (s.clock.len() >= 100).then(|| s.clock.fit()).flatten())
        {
            break f;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let start = ctx.rpc.slot().await? + 10;
    let mut leaders = vec![];
    let mut s = start;
    while s < start + slots {
        let n = (start + slots - s).min(4_000);
        leaders.extend(ctx.rpc.slot_leaders(s, n).await?);
        s += n;
    }
    let (planned, skips) = planner::plan_windows(
        start,
        &leaders,
        &targets,
        &fit,
        unix_ns(),
        &cfg,
        Some(Pubkey::new_unique()),
        &mut PlanState::default(),
    );
    println!(
        "selection {} | cluster {} | lamports/signature {lps} | clock {:.1} ms/slot (resid {:.1} ms, n={}) | budget left {}",
        sel.display(),
        ctx.cluster.name(),
        fit.ns_per_slot / 1e6,
        fit.resid_ms,
        fit.n_used,
        cfg.budget_lamports
    );
    println!(
        "{:<18} {:<18} {:>3} {:>6} {:>10} {:>3} {:>26} {:>8} {:>9}",
        "window", "stratum", "arm", "gap_ms", "first_slot", "n", "send_at (UTC)", "lead_s", "cost"
    );
    let mut total = 0;
    for w in &planned {
        total += w.expected_cost_lamports;
        let t = chrono::DateTime::from_timestamp_nanos(w.send_at_unix_ns as i64);
        println!(
            "{:<18} {:<18} {:>3} {:>6} {:>10} {:>3} {:>26} {:>8.1} {:>9}",
            w.window_id,
            w.stratum,
            format!("{:?}", w.design.arm),
            w.design.gap_us / 1000,
            w.first_slot,
            w.n_slots,
            t.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            w.lead_ms as f64 / 1000.0,
            w.expected_cost_lamports
        );
    }
    let mut by_reason: BTreeMap<String, usize> = BTreeMap::new();
    for s in &skips {
        *by_reason
            .entry(s.reason.split(':').next().unwrap_or("").to_string())
            .or_default() += 1;
    }
    println!(
        "planned {} windows, expected cost {total} lamports; skipped: {by_reason:?}",
        planned.len()
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn campaign(
    ctx: &Ctx,
    run: &RunArgs,
    run_id: &str,
    rpc_every: Option<usize>,
    mode: Mode<'_>,
    prereg: Option<String>,
    auth: Option<&LiveAuth>,
) -> Result<()> {
    let a = &run.plan;
    let sel = latest_selection(ctx, a.selection.as_ref())?;
    let targets = load_targets(ctx, &sel).await?;
    ensure!(
        !targets.is_empty(),
        "selection {} has no picked validators",
        sel.display()
    );
    let lps = lamports_per_signature(&ctx.rpc).await?;
    let rent_min = ctx.rpc.min_rent(0).await?;
    let committed = match &mode {
        Mode::Live { ledger, .. } => {
            let t = ledger.totals();
            t.spent_total + t.reserved_total
        }
        Mode::DryRun { .. } => ledger_commitment(&ctx.ledger_path())?,
    };
    let mut plan_cfg = plan_config(ctx, a, lps, committed);
    let arms_has_c = plan_cfg.arms.contains(&Arm::C);

    let (pool, sink) = match &mode {
        Mode::Live { ledger, .. } => {
            let payers = keys::load_payers(&ctx.payers_dir())?;
            ensure!(
                payers.len() >= a.n,
                "need >= {} payers in {}, found {}",
                a.n,
                ctx.payers_dir().display(),
                payers.len()
            );
            let pks: Vec<String> = payers.iter().map(|p| p.pubkey().to_string()).collect();
            let bal = ctx.rpc.balances(&pks).await?;
            let sink = if arms_has_c {
                let s = keys::read_keypair(&ctx.sink_path())
                    .context("Arm C needs secrets/sink.json (sender init-payers)")?
                    .pubkey();
                let b = ctx.rpc.balance(&s.to_string()).await?;
                ensure!(
                    b >= rent_min,
                    "sink {s} holds {b} < rent-exempt {rent_min}: run `sender fund`"
                );
                Some(s)
            } else {
                None
            };
            let busy: HashSet<String> = ledger.busy_payers().into_iter().collect();
            (
                PayerPool::new(
                    payers,
                    busy,
                    Some(pks.into_iter().zip(bal).collect()),
                    rent_min,
                ),
                sink,
            )
        }
        Mode::DryRun { .. } => {
            let payers = if ctx.payers_dir().exists() {
                keys::load_payers(&ctx.payers_dir())?
            } else {
                keys::ephemeral_payers(a.ephemeral_payers.max(a.n))
            };
            let sink = if ctx.sink_path().exists() {
                keys::read_keypair(&ctx.sink_path())?.pubkey()
            } else {
                Pubkey::new_unique()
            };
            (
                PayerPool::new(payers, HashSet::new(), None, rent_min),
                arms_has_c.then_some(sink),
            )
        }
    };
    if mode_is_live(&mode) {
        // Never plan beyond what this run may spend.
        let cap_left = auth.map(|x| x.cap()).unwrap_or(0);
        plan_cfg.budget_lamports = plan_cfg.budget_lamports.min(cap_left);
    }
    let state = warm_clock(ctx).await?;
    let cfg = RunConfig {
        run_id: run_id.into(),
        duration_secs: run.duration_secs,
        prep_ms: run.prep_ms,
        offsets_ms: run.offsets.clone(),
        rpc_every,
        simulate_every: run.simulate_every,
        selection_ref: Some(sel.display().to_string()),
        prereg,
        ..RunConfig::default()
    };
    tracing::info!(
        run_id,
        targets = targets.len(),
        payers = pool.len(),
        lps,
        rent_min,
        budget = plan_cfg.budget_lamports,
        hard_ceiling = HARD_CEILING_LAMPORTS,
        "starting campaign"
    );
    let runner = Runner::new(RunnerInit {
        rpc: ctx.rpc.clone(),
        cluster: ctx.cluster.clone(),
        data_root: ctx.data_root.clone(),
        cfg,
        plan_cfg,
        targets,
        feed: state,
        pool,
        shared_sink: sink,
        mode,
    })?;
    let dir = runner.run_dir().to_path_buf();
    let summary = runner.run().await?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    println!("logs: {}", dir.display());
    if let Some(r) = summary.stopped_early {
        bail!("run stopped early: {r}");
    }
    Ok(())
}

fn mode_is_live(m: &Mode<'_>) -> bool {
    matches!(m, Mode::Live { .. })
}

/// Resolve open reservations (crash recovery) and sent txs missing from the manifests.
/// Ledger settlement needs no spend authority: it only records what the chain shows.
async fn collect_all(ctx: &Ctx) -> Result<()> {
    let mut sent: BTreeMap<String, SentTx> = BTreeMap::new();
    let logs = ctx.data_root.join("logs");
    if logs.exists() {
        for e in std::fs::read_dir(&logs)? {
            let p = e?.path().join("sent.jsonl");
            for s in collect::read_jsonl::<SentTx>(&p)? {
                sent.insert(s.sig.clone(), s);
            }
        }
    }
    let mut in_manifest: HashSet<String> = HashSet::new();
    for purpose in [Purpose::Measure, Purpose::Calibrate] {
        for l in collect::read_jsonl::<collect::ManifestLine>(&runner::manifest_path(
            &ctx.data_root,
            purpose,
        ))? {
            in_manifest.insert(l.tx.sig);
        }
    }
    let mut ledger = Ledger::open(
        &ctx.ledger_path(),
        &format!("collect-{}", stamp()),
        HARD_CEILING_LAMPORTS,
    )?;
    let open: Vec<(String, u64)> = ledger
        .open_reservations()
        .map(|(s, _, lvbh)| (s.to_string(), lvbh))
        .collect();
    // Measurement and calibration outcomes go to their own manifests.
    for purpose in [Purpose::Measure, Purpose::Calibrate] {
        let mut c = Collector::new(
            ctx.rpc.clone(),
            &ctx.data_root,
            Some(runner::manifest_path(&ctx.data_root, purpose)),
        );
        for s in sent
            .values()
            .filter(|s| s.purpose == purpose && !in_manifest.contains(&s.sig))
        {
            c.add(Pending {
                sig: s.sig.clone(),
                last_valid_block_height: s.last_valid_block_height,
                sent: Some(s.clone()),
            });
        }
        if purpose == Purpose::Measure {
            for (sig, lvbh) in &open {
                if !sent.contains_key(sig) {
                    c.add(Pending {
                        sig: sig.clone(),
                        last_valid_block_height: *lvbh,
                        sent: None,
                    });
                }
            }
        }
        let n = c.pending_len();
        let lines = c.drain(Some(&mut ledger), Duration::from_secs(240)).await?;
        println!(
            "{purpose:?}: {n} to resolve, {} manifest lines written, {} still pending",
            lines.len(),
            c.pending_len()
        );
    }
    let t = ledger.totals();
    println!(
        "ledger: spent {} reserved {} (project ceiling {HARD_CEILING_LAMPORTS})",
        t.spent_total, t.reserved_total
    );
    Ok(())
}

async fn fund(
    ctx: &Ctx,
    payer_target: u64,
    live_flag: bool,
    max_spend: Option<u64>,
    run_id: &str,
) -> Result<()> {
    let master = NamedKeypair {
        name: "master".into(),
        keypair: keys::read_keypair(&ctx.master_path())?,
    };
    let payers = keys::load_payers(&ctx.payers_dir())?;
    let sink = load_or_create_sink(&ctx.sink_path())?;
    let rent_min = ctx.rpc.min_rent(0).await?;
    ensure!(
        payer_target > rent_min,
        "--payer-target-lamports must exceed the rent-exempt minimum {rent_min}"
    );
    let fee = funding::transfer_fee(&ctx.rpc, &master, &sink.pubkey()).await?;
    let before = funding::snapshot(&ctx.rpc, &ctx.cluster.name(), &wallets(ctx)?).await?;
    let plan = funding::plan_funding(
        &before,
        &master.pubkey().to_string(),
        payer_target,
        rent_min,
        fee,
    )?;
    let moved: u64 = plan.iter().map(|t| t.lamports).sum();
    println!(
        "{} transfers, {moved} lamports moved (locked rent, not spend), fees {} lamports",
        plan.len(),
        fee * plan.len() as u64
    );
    for t in &plan {
        println!("  {} -> {} {} (fee {})", t.from, t.to, t.lamports, t.fee);
    }
    if !live_flag {
        println!(
            "dry-run: nothing sent (add --live with CONFIRM_LIVE=yes and --max-spend-lamports)"
        );
        return Ok(());
    }
    let auth = live::authorize(
        true,
        std::env::var(live::CONFIRM_ENV).ok().as_deref(),
        max_spend,
    )?;
    let p = funding::save_snapshot(&ctx.data_root, &before)?;
    println!("opening snapshot: {}", p.display());
    let mut ledger = Ledger::open(&ctx.ledger_path(), run_id, auth.cap())?;
    let signers = BTreeMap::from([(master.pubkey().to_string(), &master)]);
    funding::execute_transfers(
        &ctx.rpc,
        &auth,
        &mut ledger,
        &ctx.data_root,
        &signers,
        &plan,
        Purpose::Fund,
    )
    .await?;
    drop(payers);
    let after = funding::snapshot(&ctx.rpc, &ctx.cluster.name(), &wallets(ctx)?).await?;
    let pa = funding::save_snapshot(&ctx.data_root, &after)?;
    let r = funding::reconcile(
        &ctx.rpc,
        &ctx.data_root,
        &ctx.ledger_path(),
        &before,
        &after,
    )
    .await?;
    println!(
        "{}\nclosing snapshot: {}",
        serde_json::to_string_pretty(&r)?,
        pa.display()
    );
    funding::fail_if_not_ok(&r)
}

async fn sweep(ctx: &Ctx, live_flag: bool, max_spend: Option<u64>, run_id: &str) -> Result<()> {
    let master_pk = keys::read_keypair(&ctx.master_path())?.pubkey().to_string();
    let payers = keys::load_payers(&ctx.payers_dir())?;
    let sink = load_or_create_sink(&ctx.sink_path())?;
    let fee = funding::transfer_fee(&ctx.rpc, &sink, &master_pk.parse()?).await?;
    let before = funding::snapshot(&ctx.rpc, &ctx.cluster.name(), &wallets(ctx)?).await?;
    let busy: std::collections::BTreeSet<String> = if ctx.ledger_path().exists() {
        Ledger::open(
            &ctx.ledger_path(),
            &format!("{run_id}-check"),
            HARD_CEILING_LAMPORTS,
        )?
        .busy_payers()
    } else {
        Default::default()
    };
    let (plan, notes) = funding::plan_sweep(&before, &master_pk, fee, &busy);
    for t in &plan {
        println!("  {} -> {} {} (fee {})", t.from, t.to, t.lamports, t.fee);
    }
    for n in &notes {
        println!("note: {n}");
    }
    if !live_flag {
        println!("dry-run: nothing sent");
        return Ok(());
    }
    let auth = live::authorize(
        true,
        std::env::var(live::CONFIRM_ENV).ok().as_deref(),
        max_spend,
    )?;
    funding::save_snapshot(&ctx.data_root, &before)?;
    let mut ledger = Ledger::open(&ctx.ledger_path(), run_id, auth.cap())?;
    let mut signers: BTreeMap<String, &NamedKeypair> =
        payers.iter().map(|p| (p.pubkey().to_string(), p)).collect();
    signers.insert(sink.pubkey().to_string(), &sink);
    funding::execute_transfers(
        &ctx.rpc,
        &auth,
        &mut ledger,
        &ctx.data_root,
        &signers,
        &plan,
        Purpose::Sweep,
    )
    .await?;
    let after = funding::snapshot(&ctx.rpc, &ctx.cluster.name(), &wallets(ctx)?).await?;
    funding::save_snapshot(&ctx.data_root, &after)?;
    let r = funding::reconcile(
        &ctx.rpc,
        &ctx.data_root,
        &ctx.ledger_path(),
        &before,
        &after,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&r)?);
    funding::fail_if_not_ok(&r)
}
