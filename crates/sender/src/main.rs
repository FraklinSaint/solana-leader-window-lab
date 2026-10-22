//! `sender`: planner, clock model, tx builder, QUIC sender and spend ledger.
//!
//! Phase 0 stub. Dry-run is the default and the only mode implemented so far; `live`
//! is refused until Phase 2 lands the spend ledger and its test suite.

use slwlab_common::{guard, limits};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("plan");
    match cmd {
        "plan" | "dry-run" => {
            println!(
                "sender (phase 0 scaffold): dry-run planner not implemented yet. \
                 ALPENGLOW_GENESIS_SLOT={} DEFAULT_MIN_LIVE_EPOCH={} HARD_CEILING_LAMPORTS={}",
                guard::ALPENGLOW_GENESIS_SLOT,
                guard::DEFAULT_MIN_LIVE_EPOCH,
                limits::HARD_CEILING_LAMPORTS
            );
        }
        "live" => {
            eprintln!("refusing: live mode is not implemented in the phase 0 scaffold");
            std::process::exit(2);
        }
        other => {
            eprintln!("unknown command {other:?}; expected plan | live");
            std::process::exit(2);
        }
    }
}
