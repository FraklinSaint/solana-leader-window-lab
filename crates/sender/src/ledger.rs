//! Write-ahead spend ledger (`data/ledger.jsonl`, append-only).
//!
//! Every fee-paying transaction is *reserved* at its worst-case fee, and the reservation is
//! fsync'ed to disk **before** the transaction bytes leave the process. Invariants, checked on
//! every reservation:
//!
//! * project: `spent_total + reserved_total <= HARD_CEILING_LAMPORTS`
//! * run:     `spent_run + reserved_run <= cap` (the CLI `--max-spend-lamports`)
//!
//! A reservation is released only on proof: the tx landed (replaced by its actual `meta.fee`)
//! or it is provably expired (block height past `lastValidBlockHeight` AND absent from
//! `getSignatureStatuses` with history search). Reservations of a crashed run stay reserved
//! after restart until resolved.

use serde::{Deserialize, Serialize};
use slwlab_common::limits::HARD_CEILING_LAMPORTS;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Measure,
    Calibrate,
    Fund,
    Sweep,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    RunStart {
        run_id: String,
        cap_lamports: u64,
        hard_ceiling_lamports: u64,
        ts: String,
    },
    Reserve {
        run_id: String,
        sig: String,
        amount: u64,
        last_valid_block_height: u64,
        purpose: Purpose,
        window_id: Option<String>,
        /// Fee payer; a payer is reused only after all its reservations are resolved.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payer: Option<String>,
        ts: String,
    },
    Landed {
        sig: String,
        fee: u64,
        slot: u64,
        ts: String,
    },
    Expired {
        sig: String,
        observed_block_height: u64,
        last_valid_block_height: u64,
        ts: String,
    },
    /// A trailing partial line found on open (crash during write). The reservation it was
    /// writing never reached fsync, so the tx was never sent (send happens after fsync).
    TornTail {
        bytes: usize,
        fragment: String,
        ts: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Reservation {
    run_id: String,
    amount: u64,
    last_valid_block_height: u64,
    payer: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Totals {
    pub spent_total: u64,
    pub reserved_total: u64,
    pub spent_run: u64,
    pub reserved_run: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt ledger line {line}: {err}")]
    Corrupt { line: usize, err: String },
    #[error(
        "cap exceeded ({scope}): spent {spent} + reserved {reserved} + new {amount} > limit {limit}"
    )]
    CapExceeded {
        scope: &'static str,
        spent: u64,
        reserved: u64,
        amount: u64,
        limit: u64,
    },
    #[error("duplicate reservation for {0}")]
    Duplicate(String),
    #[error("no open reservation for {0}")]
    Unknown(String),
    #[error("fee {fee} for {sig} exceeds its reservation {reserved}: worst-case model is wrong")]
    FeeAboveReservation {
        sig: String,
        fee: u64,
        reserved: u64,
    },
    #[error("expiry not proven for {sig}: block height {observed} <= last valid {last_valid}")]
    NotExpired {
        sig: String,
        observed: u64,
        last_valid: u64,
    },
    #[error("cap {0} exceeds the hard ceiling")]
    CapAboveCeiling(u64),
    #[error("another process holds the spend ledger ({0}); only one sender may write it at a time")]
    Locked(String),
}

/// Proof that a signature is not on chain and can never land.
pub struct ExpiryProof {
    observed_block_height: u64,
}

impl ExpiryProof {
    /// Only constructible when both conditions hold: the chain is past the blockhash's
    /// `lastValidBlockHeight` and `getSignatureStatuses(searchTransactionHistory)` returned null.
    pub fn new(
        observed_block_height: u64,
        last_valid_block_height: u64,
        status_with_history: Option<&serde_json::Value>,
    ) -> Option<Self> {
        (observed_block_height > last_valid_block_height && status_with_history.is_none())
            .then_some(Self {
                observed_block_height,
            })
    }
}

pub struct Ledger {
    path: PathBuf,
    file: File,
    /// Exclusive OS lock on `<ledger>.lock`, held for the Ledger's lifetime: two senders
    /// (two `live`, or `live` + `fund`/`sweep`/`collect`) can never write the ledger at once.
    /// Released by the OS when the process exits, even on a crash.
    _lock: File,
    run_id: String,
    cap: u64,
    open: BTreeMap<String, Reservation>,
    settled: BTreeMap<String, u64>,
    totals: Totals,
}

/// Lock file next to the ledger (`ledger.jsonl` -> `ledger.jsonl.lock`).
pub fn lock_path(ledger: &Path) -> PathBuf {
    let mut name = ledger.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    ledger.with_file_name(name)
}

fn acquire_lock(ledger: &Path) -> Result<File, LedgerError> {
    if let Some(dir) = ledger.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let lp = lock_path(ledger);
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lp)?;
    match f.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(LedgerError::Locked(lp.display().to_string()));
        }
        Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
    }
    // Holder's pid, for humans (the lock itself is what counts).
    f.set_len(0)?;
    writeln!(f, "pid {} since {}", std::process::id(), now())?;
    Ok(f)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

impl Ledger {
    /// Open (or create) the ledger, replay it, and append a `run_start` entry.
    pub fn open(path: &Path, run_id: &str, cap: u64) -> Result<Self, LedgerError> {
        if cap > HARD_CEILING_LAMPORTS {
            return Err(LedgerError::CapAboveCeiling(cap));
        }
        let lock = acquire_lock(path)?;
        // Replay (and repair a torn tail) through a read/write handle; Windows refuses
        // set_len on append-only handles. Appends then go through a separate append handle.
        let mut rw = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        let mut ledger = Self {
            path: path.to_path_buf(),
            file: OpenOptions::new().append(true).open(path)?,
            _lock: lock,
            run_id: run_id.to_string(),
            cap,
            open: BTreeMap::new(),
            settled: BTreeMap::new(),
            totals: Totals::default(),
        };
        let torn = ledger.replay(&mut rw)?;
        if let Some(bytes) = torn {
            // Cut the partial line off (keeping it verbatim in the audit entry) so the
            // file stays a sequence of complete JSON lines.
            let len = rw.metadata()?.len();
            let mut fragment = vec![0u8; bytes];
            rw.seek(SeekFrom::Start(len - bytes as u64))?;
            std::io::Read::read_exact(&mut rw, &mut fragment)?;
            rw.set_len(len - bytes as u64)?;
            rw.sync_all()?;
            ledger.append(&Entry::TornTail {
                bytes,
                fragment: String::from_utf8_lossy(&fragment).into_owned(),
                ts: now(),
            })?;
        }
        drop(rw);
        ledger.append(&Entry::RunStart {
            run_id: run_id.to_string(),
            cap_lamports: cap,
            hard_ceiling_lamports: HARD_CEILING_LAMPORTS,
            ts: now(),
        })?;
        Ok(ledger)
    }

    /// Replays complete lines; returns the byte length of a torn (newline-less) tail, if any.
    fn replay(&mut self, file: &mut File) -> Result<Option<usize>, LedgerError> {
        file.seek(SeekFrom::Start(0))?;
        let mut reader = BufReader::new(file);
        let mut buf = String::new();
        let mut line_no = 0;
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf)?;
            if n == 0 {
                return Ok(None);
            }
            line_no += 1;
            if !buf.ends_with('\n') {
                return Ok(Some(n));
            }
            let line = buf.trim_end();
            if line.is_empty() {
                continue;
            }
            let entry: Entry = serde_json::from_str(line).map_err(|e| LedgerError::Corrupt {
                line: line_no,
                err: e.to_string(),
            })?;
            self.apply(&entry).map_err(|e| LedgerError::Corrupt {
                line: line_no,
                err: e.to_string(),
            })?;
        }
    }

    fn apply(&mut self, e: &Entry) -> Result<(), LedgerError> {
        match e {
            Entry::RunStart { .. } | Entry::TornTail { .. } => {}
            Entry::Reserve {
                run_id,
                sig,
                amount,
                last_valid_block_height,
                payer,
                ..
            } => {
                if self.open.contains_key(sig) || self.settled.contains_key(sig) {
                    return Err(LedgerError::Duplicate(sig.clone()));
                }
                self.totals.reserved_total += amount;
                if *run_id == self.run_id {
                    self.totals.reserved_run += amount;
                }
                self.open.insert(
                    sig.clone(),
                    Reservation {
                        run_id: run_id.clone(),
                        amount: *amount,
                        last_valid_block_height: *last_valid_block_height,
                        payer: payer.clone(),
                    },
                );
            }
            Entry::Landed { sig, fee, .. } => {
                let r = self
                    .open
                    .remove(sig)
                    .ok_or_else(|| LedgerError::Unknown(sig.clone()))?;
                if *fee > r.amount {
                    self.open.insert(sig.clone(), r.clone());
                    return Err(LedgerError::FeeAboveReservation {
                        sig: sig.clone(),
                        fee: *fee,
                        reserved: r.amount,
                    });
                }
                self.totals.reserved_total -= r.amount;
                self.totals.spent_total += fee;
                if r.run_id == self.run_id {
                    self.totals.reserved_run -= r.amount;
                    self.totals.spent_run += fee;
                }
                self.settled.insert(sig.clone(), *fee);
            }
            Entry::Expired { sig, .. } => {
                let r = self
                    .open
                    .remove(sig)
                    .ok_or_else(|| LedgerError::Unknown(sig.clone()))?;
                self.totals.reserved_total -= r.amount;
                if r.run_id == self.run_id {
                    self.totals.reserved_run -= r.amount;
                }
                self.settled.insert(sig.clone(), 0);
            }
        }
        Ok(())
    }

    fn append(&mut self, e: &Entry) -> Result<(), LedgerError> {
        let mut line = serde_json::to_vec(e).map_err(|err| LedgerError::Corrupt {
            line: 0,
            err: err.to_string(),
        })?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.sync_data()?;
        Ok(())
    }

    /// Check both caps for `amount` without writing anything.
    pub fn check(&self, amount: u64) -> Result<(), LedgerError> {
        let t = &self.totals;
        if t.spent_total + t.reserved_total + amount > HARD_CEILING_LAMPORTS {
            return Err(LedgerError::CapExceeded {
                scope: "project hard ceiling",
                spent: t.spent_total,
                reserved: t.reserved_total,
                amount,
                limit: HARD_CEILING_LAMPORTS,
            });
        }
        if t.spent_run + t.reserved_run + amount > self.cap {
            return Err(LedgerError::CapExceeded {
                scope: "run cap",
                spent: t.spent_run,
                reserved: t.reserved_run,
                amount,
                limit: self.cap,
            });
        }
        Ok(())
    }

    /// Reserve `amount` for `sig`. Returns only after the entry is durably on disk.
    pub fn reserve(
        &mut self,
        sig: &str,
        amount: u64,
        last_valid_block_height: u64,
        purpose: Purpose,
        window_id: Option<&str>,
    ) -> Result<(), LedgerError> {
        self.reserve_paid_by(
            sig,
            amount,
            last_valid_block_height,
            purpose,
            window_id,
            None,
        )
    }

    /// [`Ledger::reserve`], also recording the fee payer.
    pub fn reserve_paid_by(
        &mut self,
        sig: &str,
        amount: u64,
        last_valid_block_height: u64,
        purpose: Purpose,
        window_id: Option<&str>,
        payer: Option<&str>,
    ) -> Result<(), LedgerError> {
        self.check(amount)?;
        let e = Entry::Reserve {
            run_id: self.run_id.clone(),
            sig: sig.to_string(),
            amount,
            last_valid_block_height,
            purpose,
            window_id: window_id.map(str::to_string),
            payer: payer.map(str::to_string),
            ts: now(),
        };
        self.apply(&e)?;
        self.append(&e)
    }

    /// Settle a landed transaction with its on-chain `meta.fee`.
    pub fn settle_landed(&mut self, sig: &str, fee: u64, slot: u64) -> Result<(), LedgerError> {
        let e = Entry::Landed {
            sig: sig.to_string(),
            fee,
            slot,
            ts: now(),
        };
        self.apply(&e)?;
        self.append(&e)
    }

    /// Release a reservation for a transaction that provably can never land.
    pub fn settle_expired(&mut self, sig: &str, proof: ExpiryProof) -> Result<(), LedgerError> {
        let r = self
            .open
            .get(sig)
            .ok_or_else(|| LedgerError::Unknown(sig.to_string()))?;
        if proof.observed_block_height <= r.last_valid_block_height {
            return Err(LedgerError::NotExpired {
                sig: sig.to_string(),
                observed: proof.observed_block_height,
                last_valid: r.last_valid_block_height,
            });
        }
        let e = Entry::Expired {
            sig: sig.to_string(),
            observed_block_height: proof.observed_block_height,
            last_valid_block_height: r.last_valid_block_height,
            ts: now(),
        };
        self.apply(&e)?;
        self.append(&e)
    }

    pub fn totals(&self) -> &Totals {
        &self.totals
    }

    pub fn cap(&self) -> u64 {
        self.cap
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Unresolved reservations (including those of earlier, crashed runs).
    pub fn open_reservations(&self) -> impl Iterator<Item = (&str, u64, u64)> {
        self.open
            .iter()
            .map(|(s, r)| (s.as_str(), r.amount, r.last_valid_block_height))
    }

    pub fn settled_fee(&self, sig: &str) -> Option<u64> {
        self.settled.get(sig).copied()
    }

    pub fn is_open(&self, sig: &str) -> bool {
        self.open.contains_key(sig)
    }

    /// Payers that still have an unresolved reservation (busy, must not be reused).
    pub fn busy_payers(&self) -> std::collections::BTreeSet<String> {
        self.open.values().filter_map(|r| r.payer.clone()).collect()
    }

    /// Read every complete entry of a ledger file (for reconciliation and audits).
    pub fn read_entries(path: &Path) -> Result<Vec<Entry>, LedgerError> {
        let text = std::fs::read_to_string(path)?;
        text.lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| {
                serde_json::from_str(l).map_err(|e| LedgerError::Corrupt {
                    line: i + 1,
                    err: e.to_string(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn open(dir: &tempfile::TempDir, run: &str, cap: u64) -> Ledger {
        Ledger::open(&dir.path().join("ledger.jsonl"), run, cap).unwrap()
    }

    #[test]
    fn busy_payers_survive_restart_until_resolved() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut l = open(&dir, "r1", 50_000);
            l.reserve_paid_by("a", 6_000, 100, Purpose::Measure, None, Some("P1"))
                .unwrap();
            l.reserve_paid_by("b", 6_000, 100, Purpose::Measure, None, Some("P2"))
                .unwrap();
            l.settle_landed("b", 5_001, 7).unwrap();
        }
        let mut l = open(&dir, "r2", 50_000);
        assert_eq!(l.busy_payers().into_iter().collect::<Vec<_>>(), vec!["P1"]);
        let proof = ExpiryProof::new(101, 100, None).unwrap();
        l.settle_expired("a", proof).unwrap();
        assert!(l.busy_payers().is_empty());
    }

    #[test]
    fn reserve_entries_without_payer_still_parse() {
        let line = r#"{"kind":"reserve","run_id":"r","sig":"s","amount":1,"last_valid_block_height":2,"purpose":"measure","window_id":null,"ts":"t"}"#;
        let e: Entry = serde_json::from_str(line).unwrap();
        assert!(matches!(e, Entry::Reserve { payer: None, .. }));
    }

    fn expired(bh: u64) -> ExpiryProof {
        ExpiryProof::new(bh, 0, None).unwrap()
    }

    #[test]
    fn cap_above_ceiling_rejected() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(
            Ledger::open(&d.path().join("l"), "r", HARD_CEILING_LAMPORTS + 1),
            Err(LedgerError::CapAboveCeiling(_))
        ));
    }

    #[test]
    fn run_cap_enforced() {
        let d = tempfile::tempdir().unwrap();
        let mut l = open(&d, "r1", 10_000);
        l.reserve("a", 6_000, 100, Purpose::Measure, None).unwrap();
        assert!(matches!(
            l.reserve("b", 5_000, 100, Purpose::Measure, None),
            Err(LedgerError::CapExceeded {
                scope: "run cap",
                ..
            })
        ));
        // landing at a lower actual fee frees headroom
        l.settle_landed("a", 5_000, 1).unwrap();
        l.reserve("b", 5_000, 100, Purpose::Measure, None).unwrap();
    }

    #[test]
    fn hard_ceiling_spans_runs() {
        let d = tempfile::tempdir().unwrap();
        {
            let mut l = open(&d, "r1", HARD_CEILING_LAMPORTS);
            l.reserve(
                "a",
                HARD_CEILING_LAMPORTS - 1_000,
                100,
                Purpose::Measure,
                None,
            )
            .unwrap();
            l.settle_landed("a", HARD_CEILING_LAMPORTS - 1_000, 1)
                .unwrap();
        }
        let mut l = open(&d, "r2", HARD_CEILING_LAMPORTS);
        assert!(matches!(
            l.reserve("b", 1_001, 100, Purpose::Measure, None),
            Err(LedgerError::CapExceeded {
                scope: "project hard ceiling",
                ..
            })
        ));
        l.reserve("b", 1_000, 100, Purpose::Measure, None).unwrap();
    }

    #[test]
    fn crash_keeps_reservations_until_resolved() {
        let d = tempfile::tempdir().unwrap();
        {
            let mut l = open(&d, "r1", 20_000);
            l.reserve("a", 6_000, 100, Purpose::Measure, Some("w1"))
                .unwrap();
            // process "crashes" here: no settlement
        }
        let mut l = open(&d, "r2", 20_000);
        assert_eq!(l.totals().reserved_total, 6_000);
        assert_eq!(l.open_reservations().count(), 1);
        assert!(matches!(
            l.settle_expired("a", ExpiryProof::new(100, 100, None).unwrap_or(expired(50))),
            Err(LedgerError::NotExpired { .. })
        ));
        l.settle_expired("a", expired(101)).unwrap();
        assert_eq!(l.totals().reserved_total, 0);
    }

    #[test]
    fn second_writer_is_refused_until_the_first_is_dropped() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("ledger.jsonl");
        let first = Ledger::open(&path, "live-1", 20_000).unwrap();
        // A separate open = a separate lock handle, exactly as a second process gets.
        match Ledger::open(&path, "live-2", 20_000) {
            Err(LedgerError::Locked(p)) => assert!(p.ends_with("ledger.jsonl.lock"), "{p}"),
            Err(e) => panic!("unexpected error {e}"),
            Ok(_) => panic!("second writer was allowed"),
        }
        // From another thread too (no in-process shortcut).
        let p2 = path.clone();
        let other = std::thread::spawn(move || Ledger::open(&p2, "live-3", 20_000).is_err());
        assert!(other.join().unwrap());
        drop(first);
        Ledger::open(&path, "live-4", 20_000).unwrap();
    }

    #[test]
    fn second_writer_in_another_process_is_refused() {
        // Re-run this test binary as a child that tries to open the locked ledger.
        if let Ok(p) = std::env::var("SLWLAB_LEDGER_LOCK_CHILD") {
            let r = Ledger::open(Path::new(&p), "child", 20_000);
            std::process::exit(if matches!(r, Err(LedgerError::Locked(_))) {
                0
            } else {
                3
            });
        }
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("ledger.jsonl");
        let _held = Ledger::open(&path, "parent", 20_000).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ledger::tests::second_writer_in_another_process_is_refused",
            ])
            .env("SLWLAB_LEDGER_LOCK_CHILD", &path)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(0), "child process was not refused");
    }

    #[test]
    fn torn_tail_is_ignored_and_marked() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("ledger.jsonl");
        {
            let mut l = Ledger::open(&path, "r1", 20_000).unwrap();
            l.reserve("a", 6_000, 100, Purpose::Measure, None).unwrap();
        }
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(br#"{"kind":"reserve","run_id":"r1","sig":"b","amo"#)
            .unwrap();
        drop(f);
        let l = Ledger::open(&path, "r2", 20_000).unwrap();
        assert_eq!(l.totals().reserved_total, 6_000);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""kind":"torn_tail""#));
        // and the file is still fully parseable afterwards
        drop(l);
        Ledger::open(&path, "r3", 20_000).unwrap();
    }

    #[test]
    fn expiry_proof_requires_both_conditions() {
        let status = serde_json::json!({"slot": 1});
        assert!(ExpiryProof::new(101, 100, None).is_some());
        assert!(ExpiryProof::new(100, 100, None).is_none());
        assert!(ExpiryProof::new(101, 100, Some(&status)).is_none());
    }

    #[test]
    fn fee_above_reservation_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let mut l = open(&d, "r", 20_000);
        l.reserve("a", 5_000, 100, Purpose::Measure, None).unwrap();
        assert!(matches!(
            l.settle_landed("a", 5_001, 1),
            Err(LedgerError::FeeAboveReservation { .. })
        ));
        assert_eq!(l.totals().reserved_total, 5_000);
    }

    #[derive(Debug, Clone)]
    enum Op {
        Reserve(u64),
        Land(usize, u64),
        Expire(usize),
        Reopen,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (1u64..2_000_000).prop_map(Op::Reserve),
            2 => (any::<usize>(), 0u64..=100).prop_map(|(i, pct)| Op::Land(i, pct)),
            1 => any::<usize>().prop_map(Op::Expire),
            1 => Just(Op::Reopen),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn cap_never_exceeded_under_random_streams(
            cap in 1u64..=HARD_CEILING_LAMPORTS,
            ops in prop::collection::vec(op(), 1..120),
        ) {
            let d = tempfile::tempdir().unwrap();
            let path = d.path().join("ledger.jsonl");
            let mut l = Ledger::open(&path, "run", cap).unwrap();
            let mut next = 0u64;
            for op in ops {
                match op {
                    Op::Reserve(a) => {
                        next += 1;
                        let before = l.totals().clone();
                        let r = l.reserve(&format!("s{next}"), a, 10, Purpose::Measure, None);
                        if r.is_err() {
                            prop_assert_eq!(l.totals(), &before); // refused => nothing changed
                        }
                    }
                    Op::Land(i, pct) => {
                        let open: Vec<_> = l.open_reservations().map(|(s, a, _)| (s.to_string(), a)).collect();
                        if !open.is_empty() {
                            let (s, a) = &open[i % open.len()];
                            l.settle_landed(s, a * pct / 100, 1).unwrap();
                        }
                    }
                    Op::Expire(i) => {
                        let open: Vec<_> = l.open_reservations().map(|(s, _, _)| s.to_string()).collect();
                        if !open.is_empty() {
                            l.settle_expired(&open[i % open.len()], expired(11)).unwrap();
                        }
                    }
                    Op::Reopen => {
                        let before = l.totals().clone();
                        drop(l);
                        l = Ledger::open(&path, "run", cap).unwrap();
                        prop_assert_eq!(l.totals(), &before);
                    }
                }
                let t = l.totals();
                prop_assert!(t.spent_run + t.reserved_run <= cap);
                prop_assert!(t.spent_total + t.reserved_total <= HARD_CEILING_LAMPORTS);
            }
        }
    }
}
