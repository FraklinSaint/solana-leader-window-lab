//! Regression test on the real data of confirmatory session 1 (run `live-20261026T220348Z`):
//! `prior_plan_state` must count the windows of that run per validator and arm exactly as the
//! manifest and `scripts/session_cmd.py` do, and the cumulative quota of session 2
//! (`alloc_calibrated_session2of4.json`) minus those counts must leave 265 windows.
//!
//! Only the reservations of that one run enter the test (the ledger is filtered to them), so
//! later sessions appended to `data/ledger.jsonl` do not change the expected numbers.

// Test-only file: clippy.toml already allows unwrap inside #[test]s.
#![allow(clippy::unwrap_used)]

use slwlab_sender::arms::Arm;
use slwlab_sender::ledger::Purpose;
use slwlab_sender::runner::prior_plan_state;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const RUN: &str = "live-20261026T220348Z";
const SELECTION: &str = "selection_20261022T163227Z.json";
const SESSION2: &str = "alloc_calibrated_session2of4.json";

/// Windows attempted in session 1 (reserved in the ledger), per identity prefix and arm,
/// counted independently from the manifest (windows with a first tx of that arm).
const EXPECTED: &[(&str, &[(Arm, usize)])] = &[
    (
        "7QQGNm3p",
        &[
            (Arm::C, 2),
            (Arm::F, 2),
            (Arm::P, 3),
            (Arm::R, 2),
            (Arm::X, 2),
        ],
    ),
    (
        "5Us18hLZ",
        &[
            (Arm::C, 1),
            (Arm::F, 2),
            (Arm::P, 2),
            (Arm::R, 1),
            (Arm::X, 1),
        ],
    ),
    (
        "DRpbCBMx",
        &[(Arm::C, 4), (Arm::F, 6), (Arm::P, 7), (Arm::X, 6)],
    ),
    (
        "EvnRmnMr",
        &[
            (Arm::C, 4),
            (Arm::F, 4),
            (Arm::P, 4),
            (Arm::R, 3),
            (Arm::X, 4),
        ],
    ),
    (
        "9eGrDohd",
        &[
            (Arm::C, 4),
            (Arm::F, 4),
            (Arm::P, 4),
            (Arm::R, 3),
            (Arm::X, 4),
        ],
    ),
    (
        "Fd7btgyS",
        &[(Arm::C, 4), (Arm::F, 6), (Arm::P, 7), (Arm::X, 6)],
    ),
    (
        "JUPiTERr",
        &[(Arm::C, 4), (Arm::F, 4), (Arm::P, 4), (Arm::X, 4)],
    ),
    (
        "HEL1USMZ",
        &[
            (Arm::C, 4),
            (Arm::F, 6),
            (Arm::P, 7),
            (Arm::R, 6),
            (Arm::X, 6),
        ],
    ),
    (
        "E1r4Psq8",
        &[
            (Arm::C, 4),
            (Arm::F, 6),
            (Arm::P, 7),
            (Arm::R, 6),
            (Arm::X, 6),
        ],
    ),
    (
        "ana2y2Yv",
        &[(Arm::C, 2), (Arm::F, 3), (Arm::P, 4), (Arm::X, 3)],
    ),
];

/// Remaining windows of session 2's file by stratum after session 1.
const REMAINING_BY_STRATUM: &[(&str, usize)] = &[
    ("Agave", 90),
    ("JitoAgave", 50),
    ("Firedancer", 38),
    ("HarmonicAgave", 20),
    ("HarmonicFiredancer", 16),
    ("BamAgave", 20),
    ("Rakurai", 31),
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn json(p: &Path) -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
    )
    .unwrap()
}

#[test]
fn session_1_counts_and_session_2_remaining_quota() {
    let data = repo().join("data");
    // Isolated copy: the run's plan.jsonl and only its ledger lines.
    let tmp = std::env::temp_dir().join(format!("slwlab-s1-plan-state-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("logs").join(RUN)).unwrap();
    std::fs::copy(
        data.join("logs").join(RUN).join("plan.jsonl"),
        tmp.join("logs").join(RUN).join("plan.jsonl"),
    )
    .unwrap();
    let ledger: String = std::fs::read_to_string(data.join("ledger.jsonl"))
        .unwrap()
        .lines()
        .filter(|l| l.contains(&format!("\"run_id\":\"{RUN}\"")))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(tmp.join("ledger.jsonl"), ledger).unwrap();

    let state = prior_plan_state(&tmp, &tmp.join("ledger.jsonl"), Purpose::Measure).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);

    // Per validator and arm.
    let mut total = 0;
    for (prefix, arms) in EXPECTED {
        let (ident, got) = state
            .arm_counts
            .iter()
            .find(|(i, _)| i.starts_with(prefix))
            .unwrap_or_else(|| panic!("no windows counted for {prefix}"));
        let want: BTreeMap<Arm, usize> = arms.iter().copied().collect();
        assert_eq!(got, &want, "{prefix}: arm counts");
        let n: usize = want.values().sum();
        assert_eq!(
            state.per_validator[ident], n,
            "{prefix}: per-validator total"
        );
        total += n;
    }
    assert_eq!(state.arm_counts.len(), EXPECTED.len(), "no other validator");
    assert_eq!(total, 188, "windows attempted in session 1");

    // Remaining quota of session 2's cumulative file.
    let sel = json(&data.join("snapshots").join(SELECTION));
    let alloc = json(&repo().join("analysis/sim/results/sessions").join(SESSION2));
    let quota = &alloc["allocation"]["plan_quota"];
    let mut left_by: BTreeMap<String, usize> = BTreeMap::new();
    for p in sel["picked"].as_array().unwrap() {
        let ident = p["identity"].as_str().unwrap();
        let stratum = p["stratum"].as_str().unwrap();
        let done = state.arm_counts.get(ident);
        let Some(q) = quota[stratum].as_object() else {
            continue;
        };
        for (arm, n) in q {
            let arm = Arm::parse(arm).unwrap();
            let d = done.and_then(|m| m.get(&arm)).copied().unwrap_or(0);
            *left_by.entry(stratum.to_string()).or_default() +=
                (n.as_u64().unwrap() as usize).saturating_sub(d);
        }
    }
    let want: BTreeMap<String, usize> = REMAINING_BY_STRATUM
        .iter()
        .map(|(s, n)| (s.to_string(), *n))
        .collect();
    assert_eq!(left_by, want, "remaining windows by stratum");
    assert_eq!(
        left_by.values().sum::<usize>(),
        265,
        "remaining windows of session 2"
    );
}
