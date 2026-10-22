//! End-to-end test against a local `solana-test-validator` (never mainnet, never devnet):
//! fund -> two live windows (Arm P over QUIC, Arm C over RPC) -> collect -> sweep -> reconcile.
//!
//! Ignored by default. Run with a validator already listening:
//!
//! ```text
//! solana-test-validator --reset --quiet &
//! SLWLAB_LOCALNET_RPC=http://127.0.0.1:8899 cargo test -p slwlab-sender -- --ignored localnet
//! ```
//!
//! CI runs it on Linux (`.github/workflows/ci.yml`, job `localnet`). solana-test-validator does
//! not start on native Windows (RocksDB, os error 5; LOGBOOK 2026-10-22).

use crate::arms::{self, Arm, ArmConfig};
use crate::cluster::Cluster;
use crate::collect::{self, ManifestLine, Status};
use crate::feed;
use crate::funding::{self, Role};
use crate::keys::{NamedKeypair, ephemeral_payers};
use crate::ledger::{Ledger, Purpose};
use crate::live::LiveAuth;
use crate::planner::{PlanConfig, PlannedWindow};
use crate::rpc::Rpc;
use crate::runner::{self, Mode, PayerPool, RunConfig, Runner, RunnerInit};
use slwlab_common::fees::total_fee;
use solana_keypair::Keypair;
use solana_signer::Signer;
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

async fn wait_balance(rpc: &Rpc, pk: &str, min: u64) {
    for _ in 0..120 {
        if rpc.balance(pk).await.unwrap_or(0) >= min {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("{pk} never reached {min} lamports");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs SLWLAB_LOCALNET_RPC pointing at a running solana-test-validator"]
async fn localnet_end_to_end() {
    let url = std::env::var("SLWLAB_LOCALNET_RPC").expect("SLWLAB_LOCALNET_RPC");
    let rpc = Rpc::new(&url);
    let cluster = Cluster::from_genesis(&rpc.genesis_hash().await.unwrap());
    assert!(
        !cluster.is_mainnet(),
        "refusing: this test sends transactions"
    );
    assert_ne!(cluster, Cluster::Devnet, "refusing: localnet only");
    let dir = tempfile::tempdir().unwrap();
    let data_root = cluster.data_root(dir.path());

    // Wallets: master (airdropped), 16 payers (two windows of 8 without reuse), Arm C sink.
    let master = NamedKeypair {
        name: "master".into(),
        keypair: Keypair::new(),
    };
    let payers = ephemeral_payers(16);
    let sink = NamedKeypair {
        name: "sink".into(),
        keypair: Keypair::new(),
    };
    rpc.request_airdrop(&master.pubkey().to_string(), 10_000_000_000)
        .await
        .unwrap();
    wait_balance(&rpc, &master.pubkey().to_string(), 10_000_000_000).await;

    let rent_min = rpc.min_rent(0).await.unwrap();
    let lps = funding::transfer_fee(&rpc, &master, &sink.pubkey())
        .await
        .unwrap();
    let auth = LiveAuth::for_tests(5_000_000);
    let ledger_path = data_root.join("ledger.jsonl");
    std::fs::create_dir_all(&data_root).unwrap();

    let mut accounts = vec![(master.pubkey(), Role::Master), (sink.pubkey(), Role::Sink)];
    accounts.extend(payers.iter().map(|p| (p.pubkey(), Role::Payer)));
    let before = funding::snapshot(&rpc, &cluster.name(), &accounts)
        .await
        .unwrap();

    // 1. fund
    {
        let mut ledger = Ledger::open(&ledger_path, "fund", auth.cap()).unwrap();
        let plan = funding::plan_funding(
            &before,
            &master.pubkey().to_string(),
            rent_min + 500_000,
            rent_min,
            lps,
        )
        .unwrap();
        assert_eq!(plan.len(), 17);
        let signers = BTreeMap::from([(master.pubkey().to_string(), &master)]);
        funding::execute_transfers(
            &rpc,
            &auth,
            &mut ledger,
            &data_root,
            &signers,
            &plan,
            Purpose::Fund,
        )
        .await
        .unwrap();
    }

    // 2. two live windows through the real runner
    let state = feed::new_state();
    feed::spawn(Some(feed::ws_from_http(&url)), rpc.clone(), state.clone());
    let nodes = rpc.cluster_nodes().await.unwrap();
    let node = &nodes[0];
    let identity = node.pubkey.clone();
    let fit = loop {
        if let Some(f) = state
            .lock()
            .ok()
            .and_then(|s| (s.clock.len() >= 10).then(|| s.clock.fit()).flatten())
        {
            break f;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let cur = rpc.slot().await.unwrap();
    let arm_cfg = ArmConfig::default();
    let windows: Vec<PlannedWindow> = [Arm::P, Arm::C]
        .iter()
        .enumerate()
        .map(|(k, &arm)| {
            let first_slot = cur + 20 + 15 * k as u64;
            let design = arms::design(&arm_cfg, arm, 0, 7 + k as u64, Some(sink.pubkey()));
            let cost = design
                .txs
                .iter()
                .map(|t| total_fee(lps, 1, t.cu_price_micro_lamports, t.cu_limit))
                .sum();
            let predicted = fit.predict_ns(first_slot) as i128;
            PlannedWindow {
                window_id: format!("{first_slot}-local"),
                identity: identity.clone(),
                vote_account: "local".into(),
                stratum: "Localnet".into(),
                tpu_quic: node.tpu_quic.clone(),
                first_slot,
                n_slots: 4,
                epoch: 0,
                predicted_first_slot_unix_ns: predicted,
                send_at_unix_ns: predicted,
                lead_ms: 0,
                design,
                expected_cost_lamports: cost,
            }
        })
        .collect();
    let expected_cost: u64 = windows.iter().map(|w| w.expected_cost_lamports).sum();
    let summary = {
        let mut ledger = Ledger::open(&ledger_path, "local-run", auth.cap()).unwrap();
        let pks: Vec<String> = payers.iter().map(|p| p.pubkey().to_string()).collect();
        let bal = rpc.balances(&pks).await.unwrap();
        let pool = PayerPool::new(
            payers
                .iter()
                .map(|p| NamedKeypair {
                    name: p.name.clone(),
                    keypair: p.keypair.insecure_clone(),
                })
                .collect(),
            HashSet::new(),
            Some(pks.into_iter().zip(bal).collect()),
            rent_min,
        );
        let runner = Runner::new(RunnerInit {
            rpc: rpc.clone(),
            cluster: cluster.clone(),
            data_root: data_root.clone(),
            cfg: RunConfig {
                run_id: "local-run".into(),
                rpc_every: Some(2), // window 2 goes through sendTransaction
                simulate_every: 1,
                min_clock_obs: 10,
                ..RunConfig::default()
            },
            plan_cfg: PlanConfig {
                lamports_per_signature: lps,
                offset_ms: 0,
                mainnet_guard: false,
                ..PlanConfig::default()
            },
            targets: vec![],
            feed: state.clone(),
            pool,
            shared_sink: Some(sink.pubkey()),
            mode: Mode::Live {
                auth: &auth,
                ledger: &mut ledger,
                purpose: Purpose::Calibrate,
            },
        })
        .unwrap();
        let s = runner.run_fixed(windows).await.unwrap();
        assert!(
            ledger.open_reservations().next().is_none(),
            "unresolved reservations"
        );
        s
    };
    assert_eq!(summary.windows_executed, 2, "{summary:?}");
    assert_eq!(summary.txs_sent, 16);
    assert_eq!(summary.send_errors, 0);
    assert_eq!(summary.unresolved, 0);
    assert_eq!(summary.landed, 16, "{summary:?}");
    assert_eq!(summary.spent_run_lamports, Some(expected_cost));

    let manifest: Vec<ManifestLine> =
        collect::read_jsonl(&runner::manifest_path(&data_root, Purpose::Calibrate)).unwrap();
    assert_eq!(manifest.len(), 16);
    let mut paths = BTreeMap::new();
    for l in &manifest {
        assert_eq!(l.outcome.status, Status::Landed);
        assert_eq!(l.outcome.producer.as_deref(), Some(identity.as_str()));
        assert!(l.outcome.block_index.is_some());
        assert_eq!(l.outcome.fee_paid, Some(l.tx.reserved_lamports));
        assert!(l.outcome.tx_error.is_none(), "{:?}", l.outcome.tx_error);
        assert!(collect::block_path(&data_root, l.outcome.slot.unwrap()).exists());
        *paths.entry(l.tx.send.path.clone()).or_insert(0) += 1;
    }
    assert_eq!(
        paths,
        BTreeMap::from([("quic".to_string(), 8), ("rpc".to_string(), 8)])
    );
    // CU usage of the minimal tx (feeds VERIFIED_FACTS F10): printed, and below the limit.
    let logs = std::fs::read_to_string(data_root.join("logs/local-run/windows.jsonl")).unwrap();
    for line in logs.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let sim = &v["simulation"];
        println!("simulation {}: {sim}", v["window"]["design"]["arm"]);
        let used = sim["units_consumed"].as_u64().unwrap();
        assert!(sim["err"].is_null() && used > 0 && used < sim["cu_limit"].as_u64().unwrap());
    }

    // 3. sweep everything back, then reconcile the whole session
    let mid = funding::snapshot(&rpc, &cluster.name(), &accounts)
        .await
        .unwrap();
    {
        let mut ledger = Ledger::open(&ledger_path, "sweep", auth.cap()).unwrap();
        let (plan, notes) = funding::plan_sweep(
            &mid,
            &master.pubkey().to_string(),
            lps,
            &ledger.busy_payers(),
        );
        assert!(notes.is_empty(), "{notes:?}");
        let mut signers: BTreeMap<String, &NamedKeypair> =
            payers.iter().map(|p| (p.pubkey().to_string(), p)).collect();
        signers.insert(sink.pubkey().to_string(), &sink);
        funding::execute_transfers(
            &rpc,
            &auth,
            &mut ledger,
            &data_root,
            &signers,
            &plan,
            Purpose::Sweep,
        )
        .await
        .unwrap();
    }
    let after = funding::snapshot(&rpc, &cluster.name(), &accounts)
        .await
        .unwrap();
    let report = funding::reconcile(&rpc, &data_root, &ledger_path, &before, &after)
        .await
        .unwrap();
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert!(report.ok, "{:?}", report.problems);
    assert_eq!(report.locked_rent_after, 0);
    assert_eq!(report.landed_in_range, 17 + 16 + 17);
}
