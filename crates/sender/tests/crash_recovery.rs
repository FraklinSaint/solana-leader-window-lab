//! Crash recovery of a live session, without a cluster (localnet does not run on Windows).
//!
//! A child process runs the runner's write sequence for each window (plan.jsonl at plan time,
//! then one fsync'd ledger reservation per tx, the send, then sent.jsonl) and is killed
//! abruptly (TerminateProcess / SIGKILL) at a random point. A power loss is simulated on top by
//! appending torn, newline-less fragments to the ledger, plan.jsonl and sent.jsonl: every append
//! is synced, so only the last line of a file can be torn. Then, as after a reboot:
//! - the ledger reopens, the torn tail is cut and recorded, and every reservation is intact;
//! - no tx appears in sent.jsonl without a durable reservation;
//! - the project ceiling counts the open reservations, and a reserved signature can never be
//!   reserved again, so nothing can be spent twice;
//! - the payers of unresolved txs stay busy;
//! - the allocation continues where it stopped (`prior_plan_state`), and no window at or before
//!   the last attempted one is planned again;
//! - after collect settles every reservation, `reconcile` balances wallets against the ledger.

// Test-only file: its helpers run inside #[test]s, where clippy.toml already allows unwrap.
#![allow(clippy::unwrap_used)]

use slwlab_common::limits::HARD_CEILING_LAMPORTS;
use slwlab_sender::arms::Arm;
use slwlab_sender::clock::Fit;
use slwlab_sender::collect;
use slwlab_sender::funding::{BalanceSnapshot, Role, reconcile_math};
use slwlab_sender::ledger::{Entry, ExpiryProof, Ledger, Purpose};
use slwlab_sender::planner::{self, PlanConfig, PlanState, PlannedWindow, Target};
use slwlab_sender::runner::prior_plan_state;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CHILD_ENV: &str = "SLWLAB_CRASH_CHILD_DIR";
const RUN: &str = "live-20261101T080000Z";
const CAP: u64 = 10_000_000;
const TXS: usize = 6;
const FEE: u64 = 7_000;
const START: u64 = 460_100_000;

fn fit() -> Fit {
    Fit {
        t0_ns: 1e18,
        slot0: START,
        ns_per_slot: 212e6,
        n_used: 100,
        n_rejected: 0,
        resid_ms: 1.0,
    }
}

/// V1 leads one 4-slot window in ten (8.5 s apart, above the 5 s spacing).
fn leaders() -> Vec<String> {
    (0..4_000u64)
        .map(|i| if (i / 4) % 10 == 1 { "V1" } else { "OTHER" }.to_string())
        .collect()
}

fn cfg() -> PlanConfig {
    let q: BTreeMap<Arm, usize> = [(Arm::P, 30), (Arm::C, 30), (Arm::X, 20)].into();
    PlanConfig {
        mainnet_guard: false,
        quotas: Some([("Agave".to_string(), q)].into()),
        ..PlanConfig::default()
    }
}

fn plan(state: &mut PlanState) -> Vec<PlannedWindow> {
    let t = Target {
        identity: "V1".into(),
        vote_account: "vote-V1".into(),
        stratum: "Agave".into(),
        tpu_quic: Some("1.1.1.1:8009".into()),
    };
    planner::plan_windows(
        START,
        &leaders(),
        &[t],
        &fit(),
        1e18 as i128 - 60_000_000_000,
        &cfg(),
        None,
        state,
    )
    .0
}

fn paths(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let run = root.join("logs").join(RUN);
    (
        root.join("ledger.jsonl"),
        run.join("plan.jsonl"),
        run.join("sent.jsonl"),
    )
}

/// The child: the runner's durable write order, forever, until killed.
#[test]
fn crash_child_writer() {
    let Ok(root) = std::env::var(CHILD_ENV) else {
        return; // only runs as the child of the tests below
    };
    let root = PathBuf::from(root);
    let (ledger_p, plan_p, sent_p) = paths(&root);
    let mut ledger = Ledger::open(&ledger_p, RUN, CAP).unwrap();
    let windows = plan(&mut PlanState::default());
    for w in &windows {
        collect::append_jsonl(&plan_p, &[serde_json::json!({ "window": w })]).unwrap();
    }
    for (k, w) in windows.iter().enumerate() {
        let mut sent = vec![];
        for i in 0..TXS {
            let sig = format!("{}-{i}", w.window_id);
            let payer = format!("payer{:02}", (k * TXS + i) % 32);
            ledger
                .reserve_paid_by(
                    &sig,
                    20_000,
                    1_000,
                    Purpose::Measure,
                    Some(&w.window_id),
                    Some(&payer),
                )
                .unwrap();
            sent.push(serde_json::json!({"sig": sig, "window_id": w.window_id, "payer": payer}));
            std::thread::sleep(Duration::from_millis(1));
        }
        // the burst goes out here; sent.jsonl is written after it
        collect::append_jsonl(&sent_p, &sent).unwrap();
    }
    // a child that finished every window would make the test meaningless
    std::thread::sleep(Duration::from_secs(600));
}

fn reserve_count(ledger: &Path) -> usize {
    std::fs::read_to_string(ledger)
        .map(|t| t.matches(r#""kind":"reserve""#).count())
        .unwrap_or(0)
}

fn run_and_kill(root: &Path, after_reserves: usize) {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "crash_child_writer",
            "--exact",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CHILD_ENV, root)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let (ledger_p, _, _) = paths(root);
    let end = Instant::now() + Duration::from_secs(60);
    while reserve_count(&ledger_p) < after_reserves {
        assert!(Instant::now() < end, "child made no progress");
        std::thread::sleep(Duration::from_millis(1));
    }
    child.kill().unwrap(); // abrupt: no destructors, no flush
    child.wait().unwrap();
}

fn append_torn(p: &Path, fragment: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
        .unwrap();
    f.write_all(fragment.as_bytes()).unwrap();
}

fn reserves(ledger: &Path) -> Vec<(String, u64, Option<String>, Option<String>)> {
    std::fs::read_to_string(ledger)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
        .filter_map(|e| match e {
            Entry::Reserve {
                sig,
                amount,
                window_id,
                payer,
                ..
            } => Some((sig, amount, window_id, payer)),
            _ => None,
        })
        .collect()
}

/// Returns whether the kill hit a window in flight (reserved txs missing from sent.jsonl).
fn check_recovery(root: &Path, power_loss: bool) -> bool {
    let (ledger_p, plan_p, sent_p) = paths(root);
    if power_loss {
        append_torn(
            &ledger_p,
            r#"{"kind":"reserve","run_id":"live-20261101T080000Z","sig":"46010"#,
        );
        append_torn(&plan_p, r#"{"window":{"window_id":"4601"#);
        append_torn(&sent_p, r#"{"sig":"4601"#);
    }
    let res = reserves(&ledger_p);
    let reserved: BTreeSet<String> = res.iter().map(|r| r.0.clone()).collect();
    assert!(!res.is_empty());

    // 1. allocation continues; the torn last lines are skipped, not fatal
    let prior = prior_plan_state(root, &ledger_p, Purpose::Measure).unwrap();
    let windows: BTreeSet<String> = res.iter().filter_map(|r| r.2.clone()).collect();
    let attempted: usize = prior.per_validator.values().sum();
    assert_eq!(
        attempted,
        windows.len(),
        "each window with a reservation counts once"
    );
    let arm_total: usize = prior.arm_counts.values().flat_map(|m| m.values()).sum();
    assert_eq!(arm_total, attempted);
    let last_slot = windows
        .iter()
        .map(|w| w.split('-').next().unwrap().parse::<u64>().unwrap())
        .max();
    assert_eq!(prior.considered_through, last_slot);

    // 2. no tx was "sent" without a durable reservation (sent.jsonl may lag, never lead)
    let sent: Vec<serde_json::Value> = collect::read_jsonl(&sent_p).unwrap();
    for s in &sent {
        assert!(
            reserved.contains(s["sig"].as_str().unwrap()),
            "sent without reservation: {s}"
        );
    }
    let in_flight = sent.len() < reserved.len();

    // 3. a restarted session never re-plans an attempted window and respects the quotas
    let mut state = prior.clone();
    let again = plan(&mut state);
    assert!(again.iter().all(|w| Some(w.first_slot) > last_slot));
    let mut per_arm: BTreeMap<Arm, usize> = BTreeMap::new();
    for m in state.arm_counts.values() {
        for (a, n) in m {
            *per_arm.entry(*a).or_default() += n;
        }
    }
    assert_eq!(
        per_arm,
        [(Arm::P, 30), (Arm::C, 30), (Arm::X, 20)].into(),
        "quota filled exactly once"
    );

    // 4. the ledger reopens with every reservation open; the cap counts them (no double spend)
    let mut ledger = Ledger::open(&ledger_p, "live-restart", CAP).unwrap();
    let reserved_total: u64 = res.iter().map(|r| r.1).sum();
    assert_eq!(ledger.totals().reserved_total, reserved_total);
    assert_eq!(ledger.open_reservations().count(), res.len());
    // The project hard ceiling counts the crashed run's open reservations, and a reserved
    // signature can never be reserved (or sent) again. (The per-purpose budgets in main.rs
    // `ledger_commitment` also count spent + reserved.)
    let headroom = HARD_CEILING_LAMPORTS - ledger.totals().spent_total - reserved_total;
    assert!(ledger.check(headroom + 1).is_err());
    assert!(ledger.check(headroom.min(CAP)).is_ok());
    assert!(
        ledger
            .reserve_paid_by(
                &res[0].0,
                1,
                1_000,
                Purpose::Measure,
                res[0].2.as_deref(),
                None
            )
            .is_err()
    );
    let busy: BTreeSet<String> = res.iter().filter_map(|r| r.3.clone()).collect();
    assert_eq!(ledger.busy_payers(), busy);
    if power_loss {
        let text = std::fs::read_to_string(&ledger_p).unwrap();
        assert!(text.contains(r#""kind":"torn_tail""#));
        assert!(
            text.lines()
                .all(|l| serde_json::from_str::<Entry>(l).is_ok()),
            "ledger left with a partial line"
        );
    }

    // 5. collect settles everything (half landed, half provably expired); reconcile balances
    let mut before = BTreeMap::new();
    for p in &busy {
        before.insert(p.clone(), (Role::Payer, 1_000_000u64));
    }
    let mut after = before.clone();
    for (k, (sig, _, _, payer)) in res.iter().enumerate() {
        if k % 2 == 0 {
            ledger.settle_landed(sig, FEE, 150).unwrap();
            after.get_mut(payer.as_ref().unwrap()).unwrap().1 -= FEE;
        } else {
            ledger
                .settle_expired(sig, ExpiryProof::new(1_001, 1_000, None).unwrap())
                .unwrap();
        }
    }
    assert_eq!(ledger.open_reservations().count(), 0);
    assert!(ledger.busy_payers().is_empty());
    let snap = |slot, accounts| BalanceSnapshot {
        ts: "2026-11-01T00:00:00Z".into(),
        cluster: "mainnet".into(),
        slot,
        accounts,
    };
    let entries = Ledger::read_entries(&ledger_p).unwrap();
    let r = reconcile_math(
        &snap(100, before.clone()),
        &snap(200, after.clone()),
        &entries,
    );
    assert!(r.ok, "{:?}", r.problems);
    assert_eq!(r.open_reservations, 0);
    // and reconcile does catch a lamport that the ledger cannot explain
    let mut off = after;
    off.values_mut().next().unwrap().1 -= 1;
    assert!(!reconcile_math(&snap(100, before), &snap(200, off), &entries).ok);
    in_flight
}

#[test]
fn abrupt_kill_mid_session() {
    if std::env::var(CHILD_ENV).is_ok() {
        return;
    }
    let mut mid_window = 0;
    for after in [1, 9, 40] {
        let dir = tempfile::tempdir().unwrap();
        run_and_kill(dir.path(), after);
        mid_window += usize::from(check_recovery(dir.path(), false));
    }
    assert!(mid_window > 0, "no kill landed inside a window");
}

#[test]
fn power_loss_mid_window() {
    if std::env::var(CHILD_ENV).is_ok() {
        return;
    }
    for after in [3, 23] {
        let dir = tempfile::tempdir().unwrap();
        run_and_kill(dir.path(), after);
        assert!(
            check_recovery(dir.path(), true),
            "power loss must hit a window in flight"
        );
    }
}
