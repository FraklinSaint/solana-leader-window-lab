//! `sender`: planner, clock model, tx builder, QUIC sender and spend ledger.
//!
//! Dry-run is the default everywhere. Only `live` and `fund/sweep --live` can move lamports,
//! and each needs `CONFIRM_LIVE=yes` plus an explicit `--max-spend-lamports` that cannot exceed
//! the compile-time ceiling (see `live::authorize`).

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use slwlab_common::guard;
use slwlab_common::limits::{
    CALIBRATION_BUDGET_LAMPORTS, CONFIRMATORY_BUDGET_LAMPORTS, HARD_CEILING_LAMPORTS,
    PLANNED_SPEND_BUDGET_LAMPORTS, RESERVE_BUDGET_LAMPORTS,
};
use slwlab_sender::arms::{Arm, ArmConfig};
use slwlab_sender::burst::unix_ns;
use slwlab_sender::clock::{self, ClockParams};
use slwlab_sender::cluster::Cluster;
use slwlab_sender::collect::{self, Collector, Pending, SentTx};
use slwlab_sender::credits::{self, CreditMeter};
use slwlab_sender::funding::{self, Role};
use slwlab_sender::keys::{self, NamedKeypair};
use slwlab_sender::ledger::{Entry, Ledger, Purpose};
use slwlab_sender::live::{self, LiveAuth};
use slwlab_sender::planner::{self, PlanConfig, PlanState, Target};
use slwlab_sender::ratelimit::{MAX_RPC_PER_SEC, MAX_SEND_TX_PER_SEC};
use slwlab_sender::redact::{RedactingWriter, redact};
use slwlab_sender::rpc::Rpc;
use slwlab_sender::rpc::RpcLimits;
use slwlab_sender::runner::{self, Mode, PayerPool, RunConfig, Runner, RunnerInit};
use slwlab_sender::select;
use slwlab_sender::session::{self, SessionMode, SessionState, Start, Worker};
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

/// Endpoints come only from the environment (or a local `.env`), never from flags or
/// code: the URL carries the provider API key, and flags end up in shell history.
#[derive(Clone)]
struct Endpoints {
    /// `RPC_URL`, HTTP JSON-RPC.
    rpc: String,
    /// `RPC_WS_URL`, slot feed (Helius: the LaserStream WebSocket). Default: derived
    /// from `RPC_URL` (https -> wss).
    ws: Option<String>,
}

impl Endpoints {
    /// The environment wins over `.env` (read by [`read_dotenv`] for runs outside compose).
    fn from_env(dotenv: &HashMap<String, String>) -> Result<Self> {
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .or_else(|| dotenv.get(k).cloned())
                .filter(|v| !v.trim().is_empty())
        };
        let rpc =
            get("RPC_URL").context("RPC_URL is not set (environment or .env; see .env.example)")?;
        Ok(Self {
            rpc,
            ws: get("RPC_WS_URL"),
        })
    }
}

/// Shared rate-limit state: `SLWLAB_RATELIMIT_DIR`, else `<data_dir>/.ratelimit`
/// (the same default as `scripts/ratelimit.py`).
fn ratelimit_dir(data_dir: &Path) -> PathBuf {
    std::env::var_os("SLWLAB_RATELIMIT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join(".ratelimit"))
}

/// Minimal `.env` reader: KEY=VALUE lines, `#` comments. Values are never printed.
fn read_dotenv(path: &Path) -> HashMap<String, String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

#[derive(Args, Clone)]
struct Global {
    /// Sustained RPC request rate (token bucket); can only be lowered from 8.
    #[arg(long, env = "RPC_MAX_RPS", default_value_t = MAX_RPC_PER_SEC, global = true)]
    rpc_rps: f64,
    /// sendTransaction rate (Helius free plan: 1/s); can only be lowered.
    #[arg(long, env = "RPC_SEND_TX_RPS", default_value_t = MAX_SEND_TX_PER_SEC, global = true)]
    send_tx_rps: f64,
    /// Slot clock: observations in the slope window (clock.rs).
    #[arg(long, default_value_t = clock::WINDOW, global = true)]
    clock_window: usize,
    /// Slot clock: recent observations that anchor the intercept (clock.rs).
    #[arg(long, default_value_t = clock::ANCHOR_K, global = true)]
    clock_anchor_k: usize,
    /// Monthly provider credit quota for the 50% warning (Helius free: 1,000,000).
    #[arg(long, env = "RPC_MONTHLY_CREDITS", default_value_t = credits::FREE_PLAN_MONTHLY_CREDITS, global = true)]
    monthly_credits: u64,
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
    /// Txs per window (pre-registered: 6, docs/SIMULATION.md).
    #[arg(long, default_value_t = 6)]
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
    /// Power-balanced allocation (analysis/sim/results/alloc_v1.json): per-stratum arm quotas.
    /// Required for `live --purpose measure`.
    #[arg(long)]
    allocation: Option<PathBuf>,
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

#[derive(Clone, Copy, ValueEnum)]
enum SessionModeArg {
    Dryrun,
    Calibrate,
    Measure,
}

#[derive(Subcommand)]
enum Cmd {
    /// A measurement session on this PC (README "Running natively"): pre-flight checks, no
    /// idle sleep, one supervised worker (dry-run or live) until the session's fixed end,
    /// restarted after a crash, gaps and coverage by UTC hour. Running the same command again
    /// resumes an interrupted dry-run session; a live session is never resumed.
    Session {
        #[arg(long, value_enum, default_value_t = SessionModeArg::Dryrun)]
        mode: SessionModeArg,
        /// Session length, at most 10 h.
        #[arg(long, default_value_t = 8.0)]
        hours: f64,
        /// Close the open session without resuming it.
        #[arg(long)]
        reset: bool,
        /// Live only: lamports the whole session may reserve (each worker gets what is left).
        #[arg(long)]
        max_spend_lamports: Option<u64>,
        /// Measure only: commit of the `prereg-v1` tag.
        #[arg(long)]
        prereg: Option<String>,
        /// Measure only: allocation file with plan_quota.
        #[arg(long)]
        allocation: Option<PathBuf>,
        /// Calibrate only: offset sweep, e.g. -300,-100,0,100.
        #[arg(long, value_delimiter = ',', allow_hyphen_values = true)]
        offsets: Vec<i64>,
        /// NTP server for the clock-offset check.
        #[arg(long, default_value = "time.cloudflare.com")]
        ntp_server: String,
    },
    /// Snapshot cluster/vote/Kobe/leader-schedule data and pick validators per stratum.
    SelectValidators {
        #[arg(long, default_value_t = 40)]
        min_windows: usize,
    },
    /// Save the leader schedule of the current and the next epoch (read-only, write-once).
    /// Unlike select-validators it writes no new selection, so the chosen validators stay.
    LeaderSnapshots,
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
    /// Devnet/localnet only: request an airdrop to the master wallet.
    Airdrop {
        #[arg(long, default_value_t = 1_000_000_000)]
        lamports: u64,
    },
    /// Create payer keypairs, the Arm C sink and (if missing) the master wallet under the
    /// secrets dir. Prints pubkeys only.
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
    /// Merge dry-run directories into a per-UTC-hour clock-error report (EXPLORATORY).
    ClockReport {
        /// Include `logs/dryrun-<stamp>` directories with stamp >= this (e.g. 20261022T200000Z).
        #[arg(long)]
        since: String,
        /// Exclude directories with stamp >= this.
        #[arg(long)]
        until: Option<String>,
        #[arg(long)]
        out: PathBuf,
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
    ep: Endpoints,
    rpc: Rpc,
    cluster: Cluster,
    data_root: PathBuf,
}

impl Ctx {
    fn clock_params(&self) -> ClockParams {
        ClockParams {
            window: self.g.clock_window,
            anchor_k: self.g.clock_anchor_k,
        }
    }
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

/// Append-mode file shared by the tracing writer; each event is one `write_all`.
struct SharedFile(std::sync::Arc<std::fs::File>);

impl std::io::Write for SharedFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        (&*self.0).write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        (&*self.0).flush()
    }
}

fn init_tracing(log_file: Option<&Path>) -> Result<()> {
    use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};
    let filter = || EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    // Both writers redact api-key parameters (redact.rs), whatever ends up in a message.
    let stderr = fmt::layer()
        .with_writer(|| RedactingWriter::new(std::io::stderr()))
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
                    .with_writer({
                        let f = std::sync::Arc::new(f);
                        move || RedactingWriter::new(SharedFile(f.clone()))
                    })
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
        let e = redact(&format!("{e:#}")).into_owned();
        // Also into the run log, so refused live attempts are auditable.
        tracing::error!(error = %e, "exiting with error");
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn real_main(cli: Cli) -> Result<()> {
    rpc::init_crypto();
    // SLWLAB_ENV_FILE selects another git-ignored file, e.g. `.env.devnet`.
    let env_file = std::env::var("SLWLAB_ENV_FILE").unwrap_or_else(|_| ".env".into());
    let ep = Endpoints::from_env(&read_dotenv(Path::new(&env_file)))?;
    ensure!(
        cli.g.rpc_rps > 0.0 && cli.g.rpc_rps <= MAX_RPC_PER_SEC,
        "--rpc-rps must be in (0, {MAX_RPC_PER_SEC}]"
    );
    ensure!(
        cli.g.send_tx_rps > 0.0 && cli.g.send_tx_rps <= MAX_SEND_TX_PER_SEC,
        "--send-tx-rps must be in (0, {MAX_SEND_TX_PER_SEC}]"
    );
    let credits_path = credits::ledger_path(&cli.g.data_dir);
    let meter = CreditMeter::new(
        credits::month_to_date(&credits_path, &credits::month_key()),
        cli.g.monthly_credits,
    );
    let rpc = Rpc::with_limits(
        &ep.rpc,
        RpcLimits {
            per_sec: cli.g.rpc_rps,
            send_tx_per_sec: cli.g.send_tx_rps,
            // One budget for every process on this machine (sender, collect, verify ...).
            shared_dir: Some(ratelimit_dir(&cli.g.data_dir)),
        },
        meter.clone(),
    );
    let genesis = rpc.genesis_hash().await.context("getGenesisHash")?;
    let cluster = Cluster::from_genesis(&genesis);
    // Before anything touches a key: refuse another cluster's secrets dir (keys.rs).
    keys::guard_secrets_cluster(&cli.g.secrets_dir, &cluster.name())?;
    if let Ok(m) = std::env::var("MASTER_KEYPAIR")
        && let Some(parent) = Path::new(&m).parent()
        && parent.join(keys::CLUSTER_MARKER).exists()
    {
        keys::guard_secrets_cluster(parent, &cluster.name())?;
    }
    let ctx = Ctx {
        data_root: cluster.data_root(&cli.g.data_dir),
        g: cli.g,
        ep,
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
    tracing::info!(
        cluster = %ctx.cluster.name(),
        data_root = %ctx.data_root.display(),
        endpoint = %ctx.rpc.endpoint(),
        rpc_rps = ctx.g.rpc_rps,
        send_tx_rps = ctx.g.send_tx_rps,
        month_to_date_credits = meter.month_to_date(),
        "connected"
    );
    let res = dispatch(&ctx, cmd, &run_id).await;
    // Credits are recorded whatever the outcome: a failed run still spent them.
    let usage = meter.usage();
    tracing::info!(
        run_id,
        credits = usage.credits,
        ws_bytes = usage.ws_bytes,
        rate_limited = usage.rate_limited,
        month_to_date_credits = meter.month_to_date(),
        quota = ctx.g.monthly_credits,
        "estimated provider credits"
    );
    if let Err(e) = meter.record(&credits_path, &run_id, &ctx.rpc.endpoint()) {
        tracing::warn!(error = %format!("{e:#}"), "could not write the credits ledger");
    }
    res
}

async fn dispatch(ctx: &Ctx, cmd: Cmd, run_id: &str) -> Result<()> {
    match cmd {
        Cmd::SelectValidators { min_windows } => select_validators(ctx, min_windows).await,
        Cmd::LeaderSnapshots => leader_snapshots(ctx).await,
        Cmd::Plan { plan, slots } => plan_only(ctx, &plan, slots).await,
        Cmd::DryRun { run, probe_connect } => {
            campaign(
                ctx,
                &run,
                run_id,
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
            // Calibration and confirmatory windows: one registered Linux platform (host.rs).
            let here = slwlab_sender::host::HostInfo::detect();
            slwlab_sender::host::gate_live(
                &here,
                purpose,
                ctx.cluster.is_mainnet(),
                &slwlab_sender::host::calibration_hosts(&runner::manifest_path(
                    &ctx.data_root,
                    Purpose::Calibrate,
                ))?,
            )?;
            let mut ledger = Ledger::open(&ctx.ledger_path(), run_id, auth.cap())?;
            report_open_reservations(&ledger);
            campaign(
                ctx,
                &run,
                run_id,
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
        Cmd::Collect => collect_all(ctx).await,
        Cmd::InitPayers { count } => {
            // New keys belong to this cluster only.
            keys::bind_secrets_cluster(&ctx.g.secrets_dir, &ctx.cluster.name())?;
            let pks = keys::init_payers(&ctx.payers_dir(), count)?;
            for (i, p) in pks.iter().enumerate() {
                println!("payer_{i:03} {p}");
            }
            let sink = load_or_create_sink(&ctx.sink_path())?;
            println!("sink {}", sink.pubkey());
            let mp = ctx.master_path();
            if !mp.exists() {
                if let Some(d) = mp.parent() {
                    std::fs::create_dir_all(d)?;
                }
                keys::write_new_keypair(&mp, &solana_keypair::Keypair::new())?;
                println!(
                    "master {} (new; fund this wallet)",
                    keys::read_keypair(&mp)?.pubkey()
                );
            } else {
                println!("master {} (existing)", keys::read_keypair(&mp)?.pubkey());
            }
            Ok(())
        }
        Cmd::Airdrop { lamports } => {
            ensure!(
                !ctx.cluster.is_mainnet(),
                "airdrop exists only on devnet/localnet; this endpoint is mainnet"
            );
            let master = keys::read_keypair(&ctx.master_path())
                .context("no master keypair: run init-payers first")?
                .pubkey()
                .to_string();
            let sig = ctx.rpc.request_airdrop(&master, lamports).await.context(
                "requestAirdrop failed (provider or faucet limit); use https://faucet.solana.com",
            )?;
            println!("airdrop {lamports} lamports to {master}: {sig}");
            Ok(())
        }
        Cmd::Balances => {
            let s = funding::snapshot(&ctx.rpc, &ctx.cluster.name(), &wallets(ctx)?).await?;
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
        } => fund(ctx, payer_target_lamports, live, max_spend_lamports, run_id).await,
        Cmd::Sweep {
            live,
            max_spend_lamports,
        } => sweep(ctx, live, max_spend_lamports, run_id).await,
        Cmd::ClockReport { since, until, out } => clock_report(ctx, &since, until.as_deref(), &out),
        Cmd::Session {
            mode,
            hours,
            reset,
            max_spend_lamports,
            prereg,
            allocation,
            offsets,
            ntp_server,
        } => {
            let mode = match mode {
                SessionModeArg::Dryrun => SessionMode::Dryrun,
                SessionModeArg::Calibrate => SessionMode::Calibrate,
                SessionModeArg::Measure => SessionMode::Measure,
            };
            run_session(
                ctx,
                SessionArgs {
                    mode,
                    hours,
                    reset,
                    max_spend_lamports,
                    prereg,
                    allocation,
                    offsets,
                    ntp_server,
                },
            )
            .await
        }
        Cmd::Reconcile { since, until } => {
            let before = funding::load_snapshot(&since)?;
            let after = match until {
                Some(p) => funding::load_snapshot(&p)?,
                None => {
                    let s =
                        funding::snapshot(&ctx.rpc, &ctx.cluster.name(), &wallets(ctx)?).await?;
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

async fn leader_snapshots(ctx: &Ctx) -> Result<()> {
    let t0 = slwlab_sender::burst::unix_ns();
    let epoch = ctx.rpc.epoch_info().await?;
    let t1 = slwlab_sender::burst::unix_ns();
    // Slot <-> UTC anchor for planning (analysis/slwlab/sessions.py): the reply's slot and
    // the local request/response times. The local clock must be NTP-synced.
    let dir = ctx.data_root.join("snapshots");
    std::fs::create_dir_all(&dir)?;
    let anchor = serde_json::json!({
        "label": "SNAPSHOT",
        "cluster": ctx.cluster.name(),
        "method": "getEpochInfo",
        "fetched_utc": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "request_unix_ns": t0.to_string(),
        "response_unix_ns": t1.to_string(),
        "absolute_slot": epoch.absolute_slot,
        "epoch": epoch.epoch,
        "slot_index": epoch.slot_index,
        "slots_in_epoch": epoch.slots_in_epoch,
    });
    std::fs::write(
        dir.join(format!("epoch_info_{}.json", stamp())),
        serde_json::to_vec_pretty(&anchor)?,
    )?;
    let emap = slwlab_sender::leaders::EpochMap::from_info(&epoch);
    let start = epoch.absolute_slot - epoch.slot_index;
    let mut failed = 0;
    for s in [start, start + epoch.slots_in_epoch] {
        match slwlab_sender::leaders::ensure(
            &ctx.rpc,
            &ctx.data_root,
            &ctx.cluster.name(),
            &emap,
            s,
        )
        .await
        {
            Ok((e, written)) => println!(
                "epoch {e}: {}",
                if written {
                    "snapshot saved"
                } else {
                    "already on disk"
                }
            ),
            Err(e) => {
                failed += 1;
                // The next epoch's schedule is published only after its leader-schedule slot.
                println!("epoch of slot {s}: not saved ({e:#})");
            }
        }
    }
    if failed == 2 {
        anyhow::bail!("no leader schedule saved");
    }
    Ok(())
}

struct SessionArgs {
    mode: SessionMode,
    hours: f64,
    reset: bool,
    max_spend_lamports: Option<u64>,
    prereg: Option<String>,
    allocation: Option<PathBuf>,
    offsets: Vec<i64>,
    ntp_server: String,
}

/// Runs workers as child processes of this same binary (the file Windows keeps locked while it
/// runs, so it cannot change during the session).
struct ProcWorker {
    exe: PathBuf,
    global: Vec<String>,
    platform: Option<String>,
    st: SessionState,
    ledger: PathBuf,
    /// The session's events.jsonl, handed to the worker (session::EVENTS_ENV).
    events: PathBuf,
}

impl ProcWorker {
    fn child(&self, args: &[String]) -> Result<i32> {
        let mut c = std::process::Command::new(&self.exe);
        c.args(&self.global).args(args);
        if let Some(p) = &self.platform {
            c.env(slwlab_sender::host::PLATFORM_ENV, p);
        }
        c.env(session::EVENTS_ENV, &self.events);
        let status = tokio::task::block_in_place(|| c.status())
            .with_context(|| format!("starting {}", self.exe.display()))?;
        Ok(status.code().unwrap_or(-1))
    }
}

impl Worker for ProcWorker {
    fn run(&mut self, left_secs: i64) -> Result<i32> {
        let mut a: Vec<String> = match self.st.mode {
            SessionMode::Dryrun => vec!["dry-run".into()],
            m => vec!["live".into(), "--purpose".into(), m.name().into()],
        };
        a.extend(["--duration-secs".into(), left_secs.to_string()]);
        if self.st.mode.is_live() {
            let cap = self.cap_left()?.unwrap_or(0);
            a.extend(["--max-spend-lamports".into(), cap.to_string()]);
            if !self.st.offsets.is_empty() {
                let o: Vec<String> = self.st.offsets.iter().map(i64::to_string).collect();
                a.extend(["--offsets".into(), o.join(",")]);
            }
            if let (Some(p), Some(al)) = (&self.st.prereg, &self.st.allocation) {
                a.extend([
                    "--prereg".into(),
                    p.clone(),
                    "--allocation".into(),
                    al.clone(),
                ]);
            }
        }
        self.child(&a)
    }
    fn after_crash(&mut self) -> Result<Option<i32>> {
        if !self.st.mode.is_live() {
            return Ok(None);
        }
        self.child(&["collect".into()]).map(Some)
    }
    fn cap_left(&mut self) -> Result<Option<u64>> {
        let Some(max) = self
            .st
            .max_spend_lamports
            .filter(|_| self.st.mode.is_live())
        else {
            return Ok(None);
        };
        let used = session::reserved_since(&self.ledger, self.st.start_unix)?;
        Ok(Some(max.saturating_sub(used)))
    }
}

type RunFlags = (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
);

/// Console close (`CTRL_CLOSE_EVENT`) and system shutdown (`CTRL_SHUTDOWN_EVENT`) as a stop.
/// The supervisor logs `console_close` / `shutdown` to the session's events.jsonl at once, then
/// stops like Ctrl+C; a worker (interrupt + urgent flags) stops without its usual waits. tokio
/// keeps the OS handler thread parked, so the process lives until Windows ends it: about 5 s
/// after a close. What cannot be caught: a sign-out (Windows sends `CTRL_LOGOFF_EVENT` to
/// services only and ends interactive processes without notice), power loss, a forced kill;
/// and `CTRL_SHUTDOWN_EVENT` reaches a console process only if it has not loaded user32.dll or
/// gdi32.dll (pre-flight records whether it has). Those ends are recorded afterwards as
/// `abnormal_end` (session.rs).
#[cfg(windows)]
fn spawn_close_listeners(
    supervisor: Option<(std::sync::Arc<session::Stop>, PathBuf)>,
    run: Option<RunFlags>,
) {
    use tokio::signal::windows::{ctrl_close, ctrl_shutdown};
    let (Ok(mut close), Ok(mut shutdown)) = (ctrl_close(), ctrl_shutdown()) else {
        tracing::warn!("could not listen for console close / system shutdown");
        return;
    };
    tokio::spawn(async move {
        let (kind, reason) = tokio::select! {
            _ = close.recv() => ("console_close", "console window closed"),
            _ = shutdown.recv() => ("shutdown", "system shutdown"),
        };
        if let Some((stop, events)) = &supervisor {
            // Logged first: Windows may end the process before the orderly `stopped`.
            let _ = session::append_event(
                events,
                chrono::Utc::now().timestamp(),
                kind,
                serde_json::json!({"detail": "stopping; Windows ends the process a few seconds after this event"}),
            );
            stop.request(reason);
        }
        if let Some((interrupt, urgent)) = &run {
            urgent.store(true, std::sync::atomic::Ordering::SeqCst);
            interrupt.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        eprintln!("{reason}: stopping now");
    });
}

#[cfg(not(windows))]
fn spawn_close_listeners(
    _supervisor: Option<(std::sync::Arc<session::Stop>, PathBuf)>,
    _run: Option<RunFlags>,
) {
}

async fn run_session(ctx: &Ctx, a: SessionArgs) -> Result<()> {
    let paths = session::Paths::new(&ctx.data_root);
    let now = chrono::Utc::now().timestamp();
    // First: a session whose supervisor vanished (sign-out, power loss, kill) gets its
    // `abnormal_end`, whatever is asked next (resume, new session or --reset).
    if let Some(ev) = session::record_abnormal_end(&paths, now)? {
        println!(
            "the open session ended abnormally: last heartbeat {}, last event {} (abnormal_end recorded)",
            ev["last_heartbeat"].as_str().unwrap_or("none"),
            ev["last_event"].as_str().unwrap_or("none"),
        );
    }
    if let Some((id, age)) = session::maybe_running(&paths, now)? {
        bail!(
            "session {id} may still be running (heartbeat {age} s ago): stop it with Ctrl+C in its \
             window, or, if it died, wait until its heartbeat is {} s old",
            session::DOWN_AFTER_SECS
        );
    }
    if a.reset {
        match session::reset(&paths, now)? {
            Some(id) => println!("session {id} closed"),
            None => println!("no open session"),
        }
        return Ok(());
    }
    let env_file = std::env::var("SLWLAB_ENV_FILE").unwrap_or_else(|_| ".env".into());
    let platform = std::env::var(slwlab_sender::host::PLATFORM_ENV)
        .ok()
        .or_else(|| read_dotenv(Path::new(&env_file)).remove(slwlab_sender::host::PLATFORM_ENV))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let mut host = slwlab_sender::host::HostInfo::detect();
    if let Some(p) = &platform {
        host.platform = p.clone();
    }
    let purpose = match a.mode {
        SessionMode::Dryrun => None,
        SessionMode::Calibrate => Some(Purpose::Calibrate),
        SessionMode::Measure => Some(Purpose::Measure),
    };
    if let Some(purpose) = purpose {
        // The worker repeats every check; failing here avoids opening a session that cannot run.
        live::authorize(
            true,
            std::env::var(live::CONFIRM_ENV).ok().as_deref(),
            a.max_spend_lamports,
        )?;
        // Every payer must be funded before a session opens: an underfunded payer would be
        // skipped silently by the pool and thin out the windows.
        let payers = keys::load_payers(&ctx.payers_dir())?;
        let pks: Vec<String> = payers.iter().map(|p| p.pubkey().to_string()).collect();
        let bal = ctx.rpc.balances(&pks).await?;
        let required = funding::required_payer_balance(
            lamports_per_signature(&ctx.rpc).await?,
            ctx.rpc.min_rent(0).await?,
            &ArmConfig::default(),
        );
        funding::check_payers_funded(&pks.into_iter().zip(bal).collect::<Vec<_>>(), required)?;
        if purpose == Purpose::Measure {
            ensure!(
                ctx.cluster.is_mainnet(),
                "confirmatory measurements are mainnet-only"
            );
            ensure!(
                a.prereg.as_deref().is_some_and(|p| p.len() >= 7) && a.allocation.is_some(),
                "--mode measure needs --prereg <prereg-v1 commit> and --allocation <file>"
            );
        }
        slwlab_sender::host::gate_live(
            &host,
            purpose,
            ctx.cluster.is_mainnet(),
            &slwlab_sender::host::calibration_hosts(&runner::manifest_path(
                &ctx.data_root,
                Purpose::Calibrate,
            ))?,
        )?;
    }
    let req = session::Request {
        mode: a.mode,
        secs: (a.hours * 3600.0).round() as i64,
        host: host.clone(),
        max_spend_lamports: a.max_spend_lamports,
        prereg: a.prereg.clone(),
        allocation: a.allocation.as_ref().map(|p| p.display().to_string()),
        offsets: a.offsets.clone(),
    };
    let pf = slwlab_sender::preflight::run(platform.as_deref(), &a.ntp_server);
    for w in &pf.warnings {
        println!("WARNING: {w}");
    }
    let (st, resumed) = match session::open_or_resume(&paths, &req, now)? {
        Start::New(st) => (st, false),
        Start::Resumed(st) => (st, true),
    };
    let pf_name = if resumed {
        format!("preflight_resume_{}.json", session::stamp(now))
    } else {
        "preflight.json".into()
    };
    std::fs::write(
        paths.dir(&st.id).join(pf_name),
        serde_json::to_vec_pretty(&serde_json::json!({
            "ts": session::utc(now), "host": host, "info": pf.info, "warnings": pf.warnings,
        }))?,
    )?;
    println!(
        "{} {} session {} until {} (sender {})",
        if resumed { "resuming" } else { "starting" },
        st.mode.name(),
        st.id,
        st.end_utc,
        &host.sender_sha256[..12]
    );
    let awake = slwlab_winsys::KeepAwake::start();
    if !awake.active {
        println!("WARNING: could not block idle sleep on this OS");
    }
    session::event(
        &paths,
        &st.id,
        chrono::Utc::now().timestamp(),
        "keep_awake",
        serde_json::json!({"active": awake.active, "request": "ES_CONTINUOUS | ES_SYSTEM_REQUIRED",
            "blocks": "idle sleep only; not a sleep or sign-out requested by the user or Windows"}),
    )?;
    let heartbeat = session::Heartbeat::start(paths.heartbeat(&st.id));
    let stop = std::sync::Arc::new(session::Stop::default());
    {
        let stop = stop.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                stop.request("Ctrl+C");
            }
        });
    }
    spawn_close_listeners(Some((stop.clone(), paths.events(&st.id))), None);
    let g = &ctx.g;
    let mut worker = ProcWorker {
        exe: std::env::current_exe()?,
        global: vec![
            "--data-dir".into(),
            g.data_dir.display().to_string(),
            "--secrets-dir".into(),
            g.secrets_dir.display().to_string(),
            "--clock-window".into(),
            g.clock_window.to_string(),
            "--clock-anchor-k".into(),
            g.clock_anchor_k.to_string(),
            "--rpc-rps".into(),
            g.rpc_rps.to_string(),
            "--send-tx-rps".into(),
            g.send_tx_rps.to_string(),
        ],
        platform,
        st: st.clone(),
        ledger: ctx.ledger_path(),
        events: paths.events(&st.id),
    };
    let outcome = session::supervise(&paths, &st, &mut worker, &stop)?;
    drop(heartbeat);
    drop(awake);
    match outcome {
        session::Outcome::Done => {
            if st.mode.is_live() {
                let c = worker.child(&["collect".into()])?;
                session::event(
                    &paths,
                    &st.id,
                    chrono::Utc::now().timestamp(),
                    "collect",
                    serde_json::json!({"exit_code": c, "detail": "end of session"}),
                )?;
            }
            let md = session::finish(&paths, &ctx.data_root, &st, chrono::Utc::now().timestamp())?;
            println!(
                "session {} done; coverage by UTC hour: {}",
                st.id,
                md.display()
            );
        }
        session::Outcome::Stopped if st.mode.is_live() => println!(
            "live session {} stopped. It will not resume: `sender session --reset`, then a new \
             session continues the allocation from the ledger",
            st.id
        ),
        session::Outcome::Stopped => println!(
            "session {} stopped; run the same command again to continue it until {}",
            st.id, st.end_utc
        ),
    }
    Ok(())
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
    let emap = slwlab_sender::leaders::EpochMap::from_info(&epoch);
    for s in [epoch_start, epoch_start + epoch.slots_in_epoch] {
        match slwlab_sender::leaders::ensure(
            &ctx.rpc,
            &ctx.data_root,
            &ctx.cluster.name(),
            &emap,
            s,
        )
        .await
        {
            Ok((e, true)) => println!("leader schedule snapshot: epoch {e}"),
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "leader schedule snapshot not saved")
            }
        }
    }
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
            "shared_tpu_ips": select::shared_tpu_ips(&nodes, &picked),
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

fn plan_config(ctx: &Ctx, a: &PlanArgs, lps: u64, budget: u64) -> Result<PlanConfig> {
    let quotas = match &a.allocation {
        Some(p) => Some(load_quotas(p)?),
        None => None,
    };
    Ok(PlanConfig {
        campaign_seed: a.seed,
        offset_ms: a.offset_ms,
        min_spacing_ms: a.min_spacing_ms,
        min_live_epoch: a.min_live_epoch,
        max_windows: a.max_windows.unwrap_or(usize::MAX),
        lamports_per_signature: lps,
        budget_lamports: budget,
        quotas,
        arms: a.arms.iter().map(|&x| x.into()).collect(),
        arm_cfg: ArmConfig {
            n: a.n,
            ..ArmConfig::default()
        },
        mainnet_guard: ctx.cluster.is_mainnet(),
        ..PlanConfig::default()
    })
}

/// `plan_quota` of analysis/slwlab/allocate.py output: stratum -> arm -> attempted windows.
fn load_quotas(path: &Path) -> Result<BTreeMap<String, BTreeMap<Arm, usize>>> {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).with_context(|| path.display().to_string())?)?;
    let q = v
        .get("allocation")
        .and_then(|a| a.get("plan_quota"))
        .or_else(|| v.get("plan_quota"))
        .context("allocation file has no plan_quota")?;
    let mut out = BTreeMap::new();
    for (stratum, arms) in q.as_object().context("plan_quota must be an object")? {
        let mut m = BTreeMap::new();
        for (arm, n) in arms
            .as_object()
            .context("plan_quota entries must be objects")?
        {
            let a = Arm::parse(arm).with_context(|| format!("unknown arm {arm}"))?;
            m.insert(a, n.as_u64().context("quota must be an integer")? as usize);
        }
        out.insert(stratum.clone(), m);
    }
    Ok(out)
}

/// spent + reserved per purpose over the whole project, read-only (dry-runs must not write
/// the ledger, and the live ledger's lock is on a separate file).
fn ledger_commitment(path: &Path) -> Result<BTreeMap<Purpose, u64>> {
    let mut out: BTreeMap<Purpose, u64> = BTreeMap::new();
    if !path.exists() {
        return Ok(out);
    }
    let mut open: BTreeMap<String, (Purpose, u64)> = BTreeMap::new();
    for e in Ledger::read_entries(path)? {
        match e {
            Entry::Reserve {
                sig,
                amount,
                purpose,
                ..
            } => {
                open.insert(sig, (purpose, amount));
            }
            Entry::Landed { sig, fee, .. } => {
                if let Some((p, _)) = open.remove(&sig) {
                    *out.entry(p).or_default() += fee;
                }
            }
            Entry::Expired { sig, .. } => {
                open.remove(&sig);
            }
            _ => {}
        }
    }
    for (p, a) in open.into_values() {
        *out.entry(p).or_default() += a;
    }
    Ok(out)
}

/// Remaining planning budget for `purpose`: its own budget minus what it has committed, and
/// never more than what is left of the project total.
fn purpose_budget(committed: &BTreeMap<Purpose, u64>, purpose: Purpose) -> u64 {
    let total: u64 = committed.values().sum();
    let used = |p: Purpose| committed.get(&p).copied().unwrap_or(0);
    let own = match purpose {
        Purpose::Measure => CONFIRMATORY_BUDGET_LAMPORTS.saturating_sub(used(Purpose::Measure)),
        Purpose::Calibrate => CALIBRATION_BUDGET_LAMPORTS.saturating_sub(used(Purpose::Calibrate)),
        Purpose::Fund | Purpose::Sweep => {
            RESERVE_BUDGET_LAMPORTS.saturating_sub(used(Purpose::Fund) + used(Purpose::Sweep))
        }
    };
    own.min(PLANNED_SPEND_BUDGET_LAMPORTS.saturating_sub(total))
}

/// Funding/sweep fees must fit in the reserve budget.
fn check_reserve(ctx: &Ctx, fees: u64) -> Result<()> {
    let left = purpose_budget(&ledger_commitment(&ctx.ledger_path())?, Purpose::Fund);
    ensure!(
        fees <= left,
        "funding/sweep fees {fees} exceed the remaining reserve budget {left} (RESERVE_BUDGET_LAMPORTS)"
    );
    Ok(())
}

async fn warm_clock(ctx: &Ctx) -> Result<feed::Shared> {
    let state = feed::new_state_with(ctx.clock_params());
    let ws = ctx
        .ep
        .ws
        .clone()
        .unwrap_or_else(|| feed::ws_from_http(&ctx.ep.rpc));
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
    let cfg = plan_config(ctx, a, lps, purpose_budget(&committed, Purpose::Measure))?;
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
        // what is left of the confirmatory allocation after earlier sessions
        &mut runner::prior_plan_state(&ctx.data_root, &ctx.ledger_path(), Purpose::Measure)?,
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
            t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
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
    // Dry-runs plan as if confirmatory; live plans against its own purpose's budget.
    let purpose = match &mode {
        Mode::Live { purpose, .. } => *purpose,
        Mode::DryRun { .. } => Purpose::Measure,
    };
    let committed = ledger_commitment(&ctx.ledger_path())?;
    let mut plan_cfg = plan_config(ctx, a, lps, purpose_budget(&committed, purpose))?;
    if purpose == Purpose::Measure && mode_is_live(&mode) {
            let required = funding::required_payer_balance(
                lps,
                rent_min,
                &ArmConfig {
                    n: a.n,
                    ..ArmConfig::default()
                },
            );
            funding::check_payers_funded(
                &pks.iter()
                    .cloned()
                    .zip(bal.iter().copied())
                    .collect::<Vec<_>>(),
                required,
            )?;
        ensure!(
            plan_cfg.quotas.is_some(),
            "--purpose measure requires --allocation (the pre-registered power-balanced allocation)"
        );
    }
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
        rpc_path_label: runner::rpc_path_label(&ctx.ep.rpc),
        endpoint: ctx.rpc.endpoint(),
        clock: ctx.clock_params(),
        host: Some(slwlab_sender::host::HostInfo::detect()),
        session_events: std::env::var_os(session::EVENTS_ENV).map(PathBuf::from),
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
    // Continue the allocation of earlier sessions (and of a run that crashed) for this purpose.
    let prior = match &mode {
        Mode::Live { purpose, .. } => {
            runner::prior_plan_state(&ctx.data_root, &ctx.ledger_path(), *purpose)?
        }
        Mode::DryRun { .. } => PlanState::default(),
    };
    if !prior.per_validator.is_empty() {
        tracing::info!(
            windows = prior.per_validator.values().sum::<usize>(),
            through_slot = prior.considered_through,
            "continuing the allocation of earlier runs"
        );
    }
    // First Ctrl+C: stop planning, drop windows not yet started, finish and write
    // summary.json (live: the bounded collect pass included). Second Ctrl+C: exit at once;
    // the ledger is durable before every send, and `sender collect` resolves what is open.
    // Console close / shutdown: the same, without the waits (Windows allows a few seconds).
    let interrupt = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let urgent = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        spawn_close_listeners(None, Some((interrupt.clone(), urgent.clone())));
        let flag = interrupt.clone();
        tokio::spawn(async move {
            let mut presses = 0u32;
            while tokio::signal::ctrl_c().await.is_ok() {
                presses += 1;
                if presses == 1 {
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    eprintln!(
                        "interrupt: finishing the run and writing summary.json (Ctrl+C again exits now)"
                    );
                } else {
                    eprintln!("second interrupt: exiting without summary.json");
                    std::process::exit(130);
                }
            }
        });
    }
    let runner = Runner::new(RunnerInit {
        interrupt,
        urgent,
        rpc: ctx.rpc.clone(),
        cluster: ctx.cluster.clone(),
        data_root: ctx.data_root.clone(),
        cfg,
        plan_cfg,
        targets,
        feed: state,
        pool,
        shared_sink: sink,
        prior,
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
    check_reserve(ctx, plan.iter().map(|t| t.fee).sum())?;
    let p = funding::save_snapshot(&ctx.data_root, &before)?;
    println!("opening snapshot: {}", p.display());
    let mut ledger = Ledger::open(&ctx.ledger_path(), run_id, auth.cap())?;
    let signers = BTreeMap::from([(master.pubkey().to_string(), &master)]);
    let master_pk = master.pubkey().to_string();
    let outcome = {
        let mut exec = funding::LiveTransfers {
            rpc: &ctx.rpc,
            auth: &auth,
            ledger: &mut ledger,
            data_root: &ctx.data_root,
            signers: &signers,
            purpose: Purpose::Fund,
            cluster: ctx.cluster.name(),
            accounts: wallets(ctx)?,
            planner: Box::new(|snap, _| {
                funding::plan_funding(snap, &master_pk, payer_target, rent_min, fee)
            }),
        };
        funding::send_until_landed(&mut exec, &master_pk, plan, funding::MAX_TRANSFER_RETRIES)
            .await?
    };
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
    .await?
    .with_transfers(&outcome);
    println!(
        "{}\nclosing snapshot: {}",
        serde_json::to_string_pretty(&r)?,
        pa.display()
    );
    funding::fail_if_not_ok(&r)
}

async fn sweep(ctx: &Ctx, live_flag: bool, max_spend: Option<u64>, run_id: &str) -> Result<()> {
    let master = NamedKeypair {
        name: "master".into(),
        keypair: keys::read_keypair(&ctx.master_path())?,
    };
    let master_pk = master.pubkey().to_string();
    let payers = keys::load_payers(&ctx.payers_dir())?;
    let sink = load_or_create_sink(&ctx.sink_path())?;
    let rent_min = ctx.rpc.min_rent(0).await?;
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
    let (plan, notes) = funding::plan_sweep(&before, &master_pk, fee, rent_min, &busy);
    for t in &plan {
        let by = if t.fee_payer.is_some() {
            ", fee paid by master"
        } else {
            ""
        };
        println!(
            "  {} -> {} {} (fee {}{by})",
            t.from, t.to, t.lamports, t.fee
        );
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
    check_reserve(ctx, plan.iter().map(|t| t.fee).sum())?;
    funding::save_snapshot(&ctx.data_root, &before)?;
    let mut ledger = Ledger::open(&ctx.ledger_path(), run_id, auth.cap())?;
    let mut signers: BTreeMap<String, &NamedKeypair> =
        payers.iter().map(|p| (p.pubkey().to_string(), p)).collect();
    signers.insert(sink.pubkey().to_string(), &sink);
    signers.insert(master_pk.clone(), &master);
    let outcome = {
        let mut exec = funding::LiveTransfers {
            rpc: &ctx.rpc,
            auth: &auth,
            ledger: &mut ledger,
            data_root: &ctx.data_root,
            signers: &signers,
            purpose: Purpose::Sweep,
            cluster: ctx.cluster.name(),
            accounts: wallets(ctx)?,
            planner: Box::new(|snap, led| {
                Ok(funding::plan_sweep(snap, &master_pk, fee, rent_min, &led.busy_payers()).0)
            }),
        };
        funding::send_until_landed(&mut exec, &master_pk, plan, funding::MAX_TRANSFER_RETRIES)
            .await?
    };
    let after = funding::snapshot(&ctx.rpc, &ctx.cluster.name(), &wallets(ctx)?).await?;
    funding::save_snapshot(&ctx.data_root, &after)?;
    let r = funding::reconcile(
        &ctx.rpc,
        &ctx.data_root,
        &ctx.ledger_path(),
        &before,
        &after,
    )
    .await?
    .with_transfers(&outcome);
    println!("{}", serde_json::to_string_pretty(&r)?);
    funding::fail_if_not_ok(&r)
}

fn clock_report(ctx: &Ctx, since: &str, until: Option<&str>, out: &Path) -> Result<()> {
    let logs = ctx.data_root.join("logs");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&logs)
        .with_context(|| logs.display().to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.strip_prefix("dryrun-")
                .is_some_and(|st| st >= since && until.is_none_or(|u| st < u))
        })
        .collect();
    dirs.sort();
    ensure!(
        !dirs.is_empty(),
        "no dryrun-* directories since {since} in {}",
        logs.display()
    );
    let report = slwlab_sender::gaps::hourly(&dirs)?;
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(out, serde_json::to_vec_pretty(&report)?)?;
    println!("{} runs -> {}", dirs.len(), out.display());
    Ok(())
}
