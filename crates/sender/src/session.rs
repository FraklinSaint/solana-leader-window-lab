//! A measurement session on the operator's PC (`sender session`): a fixed UTC interval during
//! which a supervisor keeps one worker (`sender dry-run` or `sender live`) running.
//!
//! Files under `<data>/sessions/`:
//! - `open.json`: the open session (git-ignored pointer; moved to `<id>/state.json` at the end);
//! - `<id>/session.json`: header (mode, interval, platform, OS build, machine id, sender SHA-256);
//! - `<id>/preflight.json`: pre-flight checks and warnings;
//! - `<id>/events.jsonl`: start, resume, run_start, run_exit, worker_down, down, stopped,
//!   cap_reached, collect, done, and from the worker feed_stale / feed_recovered (runner.rs
//!   staleness gate), every one with an RFC 3339 UTC timestamp. `down` (supervisor not
//!   running: crash, power loss, reboot) and `worker_down` (restart pause) are gaps with
//!   `from`/`to`; inside a run, the worker logs its own gaps to `<run>/gaps.jsonl`;
//! - `<id>/heartbeat` (git-ignored): unix seconds, rewritten every 30 s, so the next start can
//!   date a `down` gap;
//! - `<id>/coverage.json` + `coverage.md`: per UTC hour of the session, seconds covered by a live
//!   worker minus in-run gaps, gap seconds by kind, windows and clock error. Never filled.
//!
//! Ends that cannot be logged at the time: Windows terminates interactive processes at sign-out
//! without notice (`CTRL_LOGOFF_EVENT` goes to services only), and nothing runs at power loss or
//! after a forced kill. So the first thing `sender session` (and `--reset`) does is
//! [`record_abnormal_end`]: an open session whose heartbeat is stale and whose last event is not
//! `stopped` gets one `abnormal_end` event with its last heartbeat. A console close or system
//! shutdown is logged at once (`console_close`, `shutdown`) and then stopped as gracefully as the
//! few seconds Windows grants allow.
//!
//! Resume rules: a dry-run session is continued by running the same command again (after a
//! crash or reboot), until its fixed end. A live session (calibrate / measure) is never resumed:
//! the operator closes it with `--reset` and starts a new one, which continues the allocation
//! from the ledger (`runner::prior_plan_state`), so no attempted window is planned again.

use crate::collect;
use crate::gaps::{self, Gap};
use crate::host::HostInfo;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

pub const MAX_HOURS: f64 = 10.0;
/// A worker is not started for less than this.
pub const MIN_LEFT_SECS: i64 = 120;
/// Heartbeat older than this at a restart = the supervisor was not running.
pub const DOWN_AFTER_SECS: i64 = 90;
pub const HEARTBEAT_SECS: u64 = 30;
pub const RESTART_PAUSE_SECS: u64 = 30;
/// An hour is THIN when less than this share of the session's part of it was covered.
pub const THIN_SHARE: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMode {
    Dryrun,
    Calibrate,
    Measure,
}

impl SessionMode {
    pub fn is_live(self) -> bool {
        self != SessionMode::Dryrun
    }
    pub fn name(self) -> &'static str {
        match self {
            SessionMode::Dryrun => "dryrun",
            SessionMode::Calibrate => "calibrate",
            SessionMode::Measure => "measure",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub id: String,
    pub mode: SessionMode,
    pub start_unix: i64,
    pub end_unix: i64,
    pub start_utc: String,
    pub end_utc: String,
    pub host: HostInfo,
    #[serde(default)]
    pub max_spend_lamports: Option<u64>,
    #[serde(default)]
    pub prereg: Option<String>,
    #[serde(default)]
    pub allocation: Option<String>,
    #[serde(default)]
    pub offsets: Vec<i64>,
}

pub fn utc(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "invalid".into())
}

pub fn stamp(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|t| t.format("%Y%m%dT%H%M%SZ").to_string())
        .unwrap_or_else(|| "invalid".into())
}

fn parse_utc(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.timestamp())
}

pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn new(data_root: &Path) -> Self {
        Self {
            root: data_root.join("sessions"),
        }
    }
    pub fn open(&self) -> PathBuf {
        self.root.join("open.json")
    }
    pub fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
    pub fn events(&self, id: &str) -> PathBuf {
        self.dir(id).join("events.jsonl")
    }
    pub fn heartbeat(&self, id: &str) -> PathBuf {
        self.dir(id).join("heartbeat")
    }
}

/// Append one event (UTC timestamp first). Synced like every other jsonl append.
pub fn event(p: &Paths, id: &str, now: i64, kind: &str, fields: Value) -> Result<()> {
    append_event(&p.events(id), now, kind, fields)
}

/// Set by the supervisor on its worker: the session's `events.jsonl`, so the worker can add
/// its own events (`feed_stale`, `feed_recovered`). One short line per append.
pub const EVENTS_ENV: &str = "SLWLAB_SESSION_EVENTS";

pub fn append_event(path: &Path, now: i64, kind: &str, fields: Value) -> Result<()> {
    let mut o = serde_json::Map::new();
    o.insert("ts".into(), json!(utc(now)));
    o.insert("event".into(), json!(kind));
    if let Value::Object(m) = fields {
        o.extend(m);
    }
    collect::append_jsonl(path, &[Value::Object(o)])
}

pub fn read_events(p: &Paths, id: &str) -> Result<Vec<Value>> {
    collect::read_jsonl(&p.events(id))
}

fn write_json(path: &Path, v: &impl Serialize) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Stop request shared by the signal listeners and the supervisor loop; the first reason wins.
#[derive(Debug, Default)]
pub struct Stop {
    flag: AtomicBool,
    reason: std::sync::Mutex<Option<String>>,
}

impl Stop {
    pub fn request(&self, reason: &str) {
        if let Ok(mut r) = self.reason.lock() {
            r.get_or_insert_with(|| reason.to_string());
        }
        self.flag.store(true, Ordering::SeqCst);
    }
    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
    pub fn flag(&self) -> &AtomicBool {
        &self.flag
    }
    pub fn reason(&self) -> String {
        self.reason
            .lock()
            .ok()
            .and_then(|r| r.clone())
            .unwrap_or_else(|| "Ctrl+C".into())
    }
}

/// Last heartbeat of a session (unix seconds), if one was written.
pub fn last_heartbeat(p: &Paths, id: &str) -> Option<i64> {
    std::fs::read_to_string(p.heartbeat(id))
        .ok()
        .and_then(|t| t.trim().parse().ok())
}

/// Events after which an open session is not abnormal: the supervisor said why it stopped,
/// or the abnormal end is already recorded.
const ORDERLY_LAST: [&str; 3] = ["stopped", "abnormal_end", "reset"];

fn open_state(p: &Paths) -> Result<Option<SessionState>> {
    if !p.open().exists() {
        return Ok(None);
    }
    let st = serde_json::from_slice(&std::fs::read(p.open())?)
        .with_context(|| p.open().display().to_string())?;
    Ok(Some(st))
}

/// The open session's supervisor may still be running: its heartbeat is newer than
/// [`DOWN_AFTER_SECS`] and it has not logged `stopped`. Returns (id, heartbeat age).
pub fn maybe_running(p: &Paths, now: i64) -> Result<Option<(String, i64)>> {
    let Some(st) = open_state(p)? else {
        return Ok(None);
    };
    let Some(hb) = last_heartbeat(p, &st.id) else {
        return Ok(None);
    };
    let last = read_events(p, &st.id)?;
    let last = last.last().and_then(|e| e["event"].as_str()).unwrap_or("");
    Ok((now - hb <= DOWN_AFTER_SECS && !ORDERLY_LAST.contains(&last)).then_some((st.id, now - hb)))
}

/// Run first by `sender session` and `--reset`: if the open session's supervisor died without
/// saying so (sign-out, power loss, forced kill, crash), append one `abnormal_end` event with
/// the last heartbeat (UTC). Returns the event, or `None` if there is nothing to record
/// (no open session, a fresh heartbeat, or an orderly or already recorded last event).
pub fn record_abnormal_end(p: &Paths, now: i64) -> Result<Option<Value>> {
    let Some(st) = open_state(p)? else {
        return Ok(None);
    };
    let hb = last_heartbeat(p, &st.id);
    if hb.is_some_and(|t| now - t <= DOWN_AFTER_SECS) {
        return Ok(None);
    }
    let evs = read_events(p, &st.id)?;
    let last = evs.last();
    let last_kind = last.and_then(|e| e["event"].as_str()).unwrap_or("");
    if ORDERLY_LAST.contains(&last_kind) {
        return Ok(None);
    }
    let fields = json!({
        "last_heartbeat": hb.map(utc),
        "secs_since_heartbeat": hb.map(|t| now - t),
        "last_event": last_kind,
        "last_event_ts": last.map(|e| e["ts"].clone()),
        "session_end": st.end_utc,
        "detail": "the supervisor ended without a stopped or done event: sign-out, power loss, \
                   forced kill or crash (the cause is not recorded by the sender)",
    });
    event(p, &st.id, now, "abnormal_end", fields.clone())?;
    Ok(Some(fields))
}

/// Arguments of one worker run: `dry-run`, or `live --purpose <mode>` with the cap that is
/// left, the calibration offsets and the measurement pre-registration.
pub fn worker_args(st: &SessionState, left_secs: i64, cap_left: Option<u64>) -> Vec<String> {
    let mut a: Vec<String> = match st.mode {
        SessionMode::Dryrun => vec!["dry-run".into()],
        m => vec!["live".into(), "--purpose".into(), m.name().into()],
    };
    a.extend(["--duration-secs".into(), left_secs.to_string()]);
    if st.mode.is_live() {
        a.extend([
            "--max-spend-lamports".into(),
            cap_left.unwrap_or(0).to_string(),
        ]);
        if !st.offsets.is_empty() {
            let o: Vec<String> = st.offsets.iter().map(i64::to_string).collect();
            a.extend(["--offsets".into(), o.join(",")]);
        }
        if let (Some(p), Some(al)) = (&st.prereg, &st.allocation) {
            a.extend([
                "--prereg".into(),
                p.clone(),
                "--allocation".into(),
                al.clone(),
            ]);
        }
    }
    a
}

/// What the operator asked for.
#[derive(Debug, Clone)]
pub struct Request {
    pub mode: SessionMode,
    pub secs: i64,
    pub host: HostInfo,
    pub max_spend_lamports: Option<u64>,
    pub prereg: Option<String>,
    pub allocation: Option<String>,
    pub offsets: Vec<i64>,
}

#[derive(Debug)]
pub enum Start {
    New(SessionState),
    Resumed(SessionState),
}

/// Open a new session, or resume the open dry-run session. Never resumes a live session.
pub fn open_or_resume(p: &Paths, req: &Request, now: i64) -> Result<Start> {
    std::fs::create_dir_all(&p.root)?;
    if p.open().exists() {
        let st: SessionState = serde_json::from_slice(&std::fs::read(p.open())?)
            .with_context(|| p.open().display().to_string())?;
        if st.mode.is_live() {
            bail!(
                "{} session {} (started {}) was interrupted. Live sessions are never resumed. \
                 Close it with `sender session --reset`; a new session continues the allocation \
                 from the ledger and never re-plans an attempted window",
                st.mode.name(),
                st.id,
                st.start_utc
            );
        }
        if req.mode != st.mode {
            bail!(
                "dry-run session {} is open until {}: run `sender session --mode dryrun` to \
                 continue it, or `sender session --reset` to close it",
                st.id,
                st.end_utc
            );
        }
        if let Ok(t) = std::fs::read_to_string(p.heartbeat(&st.id))
            && let Ok(last) = t.trim().parse::<i64>()
            && now - last > DOWN_AFTER_SECS
        {
            event(
                p,
                &st.id,
                now,
                "down",
                json!({"from": utc(last), "to": utc(now), "secs": now - last,
                       "detail": "supervisor not running (crash, power loss, reboot or closed window)"}),
            )?;
        }
        event(
            p,
            &st.id,
            now,
            "resume",
            json!({"left_secs": st.end_unix - now, "host": req.host}),
        )?;
        return Ok(Start::Resumed(st));
    }
    if req.secs <= 0 || req.secs as f64 > MAX_HOURS * 3600.0 {
        bail!("a session lasts more than 0 and at most {MAX_HOURS} h");
    }
    let start = now;
    let id = stamp(start - 1);
    let st = SessionState {
        id: id.clone(),
        mode: req.mode,
        start_unix: start,
        end_unix: start + req.secs,
        start_utc: utc(start),
        end_utc: utc(start + req.secs),
        host: req.host.clone(),
        max_spend_lamports: req.max_spend_lamports,
        prereg: req.prereg.clone(),
        allocation: req.allocation.clone(),
        offsets: req.offsets.clone(),
    };
    std::fs::create_dir_all(p.dir(&id))?;
    write_json(&p.dir(&id).join("session.json"), &st)?;
    write_json(&p.open(), &st)?;
    event(
        p,
        &id,
        now,
        "start",
        json!({"mode": st.mode, "end": st.end_utc}),
    )?;
    Ok(Start::New(st))
}

/// Close the open session without resuming it (the only way out of an interrupted live one).
pub fn reset(p: &Paths, now: i64) -> Result<Option<String>> {
    if !p.open().exists() {
        return Ok(None);
    }
    let st: SessionState = serde_json::from_slice(&std::fs::read(p.open())?)?;
    event(
        p,
        &st.id,
        now,
        "reset",
        json!({"detail": "closed by the operator before its end"}),
    )?;
    close(p, &st)?;
    Ok(Some(st.id))
}

fn close(p: &Paths, st: &SessionState) -> Result<()> {
    std::fs::rename(p.open(), p.dir(&st.id).join("state.json"))?;
    let _ = std::fs::remove_file(p.heartbeat(&st.id));
    Ok(())
}

/// Rewrites `<id>/heartbeat` every 30 s until dropped.
pub struct Heartbeat {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Heartbeat {
    pub fn start(path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            loop {
                let now = chrono::Utc::now().timestamp();
                let _ = std::fs::write(&path, now.to_string());
                match rx.recv_timeout(std::time::Duration::from_secs(HEARTBEAT_SECS)) {
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    _ => break,
                }
            }
        });
        Self {
            stop: Some(tx),
            thread: Some(thread),
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// What the supervisor drives. `sender session` spawns child processes; tests use fakes.
pub trait Worker {
    /// Run one worker for at most `left_secs`; returns its exit code.
    fn run(&mut self, left_secs: i64) -> Result<i32>;
    /// After a failed live worker: resolve open reservations (`sender collect`).
    fn after_crash(&mut self) -> Result<Option<i32>> {
        Ok(None)
    }
    /// Live: lamports this session may still reserve; `None` = no cap (dry-run).
    fn cap_left(&mut self) -> Result<Option<u64>> {
        Ok(None)
    }
    fn now(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }
    /// Interruptible pause; returns false if stopped during it.
    fn pause(&mut self, secs: u64, stop: &AtomicBool) -> bool {
        for _ in 0..secs {
            if stop.load(Ordering::SeqCst) {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        !stop.load(Ordering::SeqCst)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Stopped,
}

/// Keep a worker running until the session's end; restart it after a crash.
pub fn supervise(p: &Paths, st: &SessionState, w: &mut dyn Worker, stop: &Stop) -> Result<Outcome> {
    let mut n = read_events(p, &st.id)?
        .iter()
        .filter(|e| e["event"] == "run_exit")
        .count();
    loop {
        if stop.is_set() {
            event(
                p,
                &st.id,
                w.now(),
                "stopped",
                json!({"detail": stop.reason()}),
            )?;
            return Ok(Outcome::Stopped);
        }
        let t0 = w.now();
        let left = st.end_unix - t0;
        if left < MIN_LEFT_SECS {
            return Ok(Outcome::Done);
        }
        if let Some(cap) = w.cap_left()?
            && cap == 0
        {
            event(p, &st.id, t0, "cap_reached", json!({}))?;
            return Ok(Outcome::Done);
        }
        n += 1;
        event(
            p,
            &st.id,
            t0,
            "run_start",
            json!({"run": n, "left_secs": left}),
        )?;
        let code = w.run(left)?;
        let t1 = w.now();
        event(
            p,
            &st.id,
            t1,
            "run_exit",
            json!({"run": n, "exit_code": code, "from": utc(t0), "to": utc(t1), "ran_secs": t1 - t0}),
        )?;
        if stop.is_set() {
            event(p, &st.id, t1, "stopped", json!({"detail": stop.reason()}))?;
            return Ok(Outcome::Stopped);
        }
        if code != 0 {
            let resumed = w.pause(RESTART_PAUSE_SECS, stop.flag());
            if let Some(c) = w.after_crash()? {
                event(p, &st.id, w.now(), "collect", json!({"exit_code": c}))?;
            }
            let t2 = w.now();
            event(
                p,
                &st.id,
                t2,
                "worker_down",
                json!({"from": utc(t1), "to": utc(t2), "secs": t2 - t1,
                       "detail": format!("worker exit {code}, restart pause")}),
            )?;
            if !resumed {
                event(p, &st.id, t2, "stopped", json!({"detail": stop.reason()}))?;
                return Ok(Outcome::Stopped);
            }
        }
    }
}

/// Lamports reserved in the ledger since `start_unix` (an upper bound on this session's fees).
/// A torn last line (power loss) is ignored, as everywhere else.
pub fn reserved_since(ledger: &Path, start_unix: i64) -> Result<u64> {
    let entries: Vec<Value> = collect::read_jsonl(ledger)?;
    Ok(entries
        .iter()
        .filter(|e| e["kind"] == "reserve")
        .filter(|e| {
            e["ts"]
                .as_str()
                .and_then(parse_utc)
                .is_some_and(|t| t >= start_unix)
        })
        .filter_map(|e| e["amount"].as_u64())
        .sum())
}

// ---------------------------------------------------------------- coverage

fn union(mut iv: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    iv.sort();
    let mut out: Vec<(i64, i64)> = vec![];
    for (a, b) in iv.into_iter().filter(|(a, b)| b > a) {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

fn subtract(iv: &[(i64, i64)], cut: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let cut = union(cut.to_vec());
    let mut out = vec![];
    for &(a, b) in iv {
        let mut s = a;
        for &(c0, c1) in &cut {
            if c1 <= s || c0 >= b {
                continue;
            }
            if c0 > s {
                out.push((s, c0));
            }
            s = s.max(c1);
        }
        if s < b {
            out.push((s, b));
        }
    }
    out
}

fn overlap(iv: &[(i64, i64)], h0: i64, h1: i64) -> i64 {
    iv.iter()
        .map(|&(a, b)| (b.min(h1) - a.max(h0)).max(0))
        .sum()
}

/// Run directories (`logs/dryrun-*`, `logs/live-*`) started during the session.
pub fn session_runs(data_root: &Path, st: &SessionState) -> Vec<PathBuf> {
    let (lo, hi) = (stamp(st.start_unix - 1), stamp(st.end_unix + 3600));
    let mut out: Vec<PathBuf> = std::fs::read_dir(data_root.join("logs"))
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    out.retain(|d| {
        d.is_dir()
            && d.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| {
                    n.strip_prefix("dryrun-")
                        .or_else(|| n.strip_prefix("live-"))
                })
                .is_some_and(|s| s >= lo.as_str() && s < hi.as_str())
    });
    out.sort();
    out
}

/// Coverage by UTC hour from the session's events and runs. Hours without coverage stay in the
/// table with zero seconds and flag GAP; nothing is interpolated.
pub fn coverage(p: &Paths, data_root: &Path, st: &SessionState) -> Result<Value> {
    let events = read_events(p, &st.id)?;
    let ts = |e: &Value, k: &str| e[k].as_str().and_then(parse_utc);
    let worker: Vec<(i64, i64)> = events
        .iter()
        .filter(|e| e["event"] == "run_exit")
        .filter_map(|e| Some((ts(e, "from")?, ts(e, "to")?)))
        .collect();
    let sup_gaps: Vec<(String, i64, i64)> = events
        .iter()
        .filter(|e| e["event"] == "down" || e["event"] == "worker_down")
        .filter_map(|e| {
            Some((
                e["event"].as_str()?.to_string(),
                ts(e, "from")?,
                ts(e, "to")?,
            ))
        })
        .collect();
    let runs = session_runs(data_root, st);
    let mut run_gaps: Vec<(String, i64, i64)> = vec![];
    for d in &runs {
        for g in collect::read_jsonl::<Gap>(&d.join("gaps.jsonl")).unwrap_or_default() {
            run_gaps.push((
                g.kind,
                (g.start_unix_ns / 1_000_000_000) as i64,
                (g.end_unix_ns / 1_000_000_000) as i64,
            ));
        }
    }
    let covered = subtract(
        &union(worker.clone()),
        &run_gaps.iter().map(|g| (g.1, g.2)).collect::<Vec<_>>(),
    );
    let clock = if runs.is_empty() {
        json!({"hours": []})
    } else {
        gaps::hourly(&runs)?
    };
    let by_hour: std::collections::BTreeMap<String, Value> = clock["hours"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|h| Some((h["hour_utc"].as_str()?.to_string(), h)))
        .collect();
    let mut hours = vec![];
    let mut h0 = st.start_unix - st.start_unix.rem_euclid(3600);
    while h0 < st.end_unix {
        let h1 = h0 + 3600;
        let span = h1.min(st.end_unix) - h0.max(st.start_unix);
        let cov = overlap(&covered, h0, h1);
        let mut blind = serde_json::Map::new();
        for (k, a, b) in sup_gaps.iter().chain(run_gaps.iter()) {
            let s = overlap(&[(*a, *b)], h0, h1);
            if s > 0 {
                let v = blind.get(k).and_then(Value::as_i64).unwrap_or(0) + s;
                blind.insert(k.clone(), json!(v));
            }
        }
        let key = gaps::hour_of(i128::from(h0) * 1_000_000_000);
        let c = by_hour.get(&key);
        let flag = if cov == 0 {
            "GAP"
        } else if (cov as f64) < THIN_SHARE * span as f64 {
            "THIN"
        } else {
            "OK"
        };
        hours.push(json!({
            "hour_utc": key, "session_secs": span, "covered_secs": cov, "blind_secs": blind,
            "windows_executed": c.map(|c| c["windows_executed"].clone()).unwrap_or(json!(0)),
            "windows_skipped": c.map(|c| c["windows_skipped"].clone()).unwrap_or(json!(0)),
            "clock_err": c.map(|c| c["err"].clone()).unwrap_or(Value::Null),
            "flag": flag,
        }));
        h0 = h1;
    }
    Ok(json!({
        "label": "EXPLORATORY",
        "session": st.id, "mode": st.mode, "start": st.start_utc, "end": st.end_utc,
        "metric": "covered_secs = seconds a worker was running minus its own gaps (host_stall, \
                   slot_feed, rpc_error). Hours with no coverage are GAP, never filled. clock_err: \
                   window-start prediction error at prep time (see sender clock-report).",
        "runs": runs.iter().filter_map(|d| d.file_name()?.to_str().map(str::to_string)).collect::<Vec<_>>(),
        "covered_secs_total": overlap(&covered, st.start_unix, st.end_unix),
        "hours": hours,
    }))
}

pub fn coverage_md(c: &Value) -> String {
    let mut s = format!(
        "# Session {} ({}) coverage by UTC hour (EXPLORATORY)\n\n{} to {}. Covered {:.1} h. \
         Hours flagged GAP had no coverage; nothing is filled.\n\n\
         | hour (UTC) | session min | covered min | blind (min, by kind) | windows | median abs clock err (ms) | flag |\n\
         |---|---|---|---|---|---|---|\n",
        c["session"].as_str().unwrap_or(""),
        c["mode"].as_str().unwrap_or(""),
        c["start"].as_str().unwrap_or(""),
        c["end"].as_str().unwrap_or(""),
        c["covered_secs_total"].as_f64().unwrap_or(0.0) / 3600.0
    );
    for h in c["hours"].as_array().cloned().unwrap_or_default() {
        let blind = h["blind_secs"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| format!("{k} {:.0}", v.as_f64().unwrap_or(0.0) / 60.0))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let err = h["clock_err"]["median_abs_ms"]
            .as_f64()
            .map(|x| format!("{x:.0}"))
            .unwrap_or_else(|| "–".into());
        s += &format!(
            "| {} | {:.0} | {:.0} | {} | {} | {} | {} |\n",
            h["hour_utc"].as_str().unwrap_or(""),
            h["session_secs"].as_f64().unwrap_or(0.0) / 60.0,
            h["covered_secs"].as_f64().unwrap_or(0.0) / 60.0,
            blind,
            h["windows_executed"],
            err,
            h["flag"].as_str().unwrap_or("")
        );
    }
    s
}

/// Write coverage, log `done` and close the session.
pub fn finish(p: &Paths, data_root: &Path, st: &SessionState, now: i64) -> Result<PathBuf> {
    let c = coverage(p, data_root, st)?;
    write_json(&p.dir(&st.id).join("coverage.json"), &c)?;
    std::fs::write(p.dir(&st.id).join("coverage.md"), coverage_md(&c))?;
    event(
        p,
        &st.id,
        now,
        "done",
        json!({"covered_secs": c["covered_secs_total"]}),
    )?;
    close(p, st)?;
    Ok(p.dir(&st.id).join("coverage.md"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn host() -> HostInfo {
        HostInfo {
            platform: "win-pc1".into(),
            os: "windows".into(),
            arch: "x86_64".into(),
            os_build: "10.0.17763.9376".into(),
            machine_id: "0123456789abcdef".into(),
            sender_sha256: "ab".repeat(32),
        }
    }

    fn req(mode: SessionMode, secs: i64) -> Request {
        Request {
            mode,
            secs,
            host: host(),
            max_spend_lamports: mode.is_live().then_some(1_000_000),
            prereg: None,
            allocation: None,
            offsets: vec![],
        }
    }

    /// Fake worker on a fake clock: each run lasts `run_secs` and exits with the next code.
    struct Fake<'a> {
        clock: &'a Cell<i64>,
        codes: Vec<i32>,
        run_secs: i64,
        runs: Vec<i64>,
        collects: usize,
        cap: Option<u64>,
    }

    impl Worker for Fake<'_> {
        fn run(&mut self, left: i64) -> Result<i32> {
            self.runs.push(left);
            self.clock.set(self.clock.get() + self.run_secs.min(left));
            Ok(if self.codes.is_empty() {
                0
            } else {
                self.codes.remove(0)
            })
        }
        fn after_crash(&mut self) -> Result<Option<i32>> {
            if self.cap.is_some() {
                self.collects += 1;
                return Ok(Some(0));
            }
            Ok(None)
        }
        fn cap_left(&mut self) -> Result<Option<u64>> {
            Ok(self.cap)
        }
        fn now(&self) -> i64 {
            self.clock.get()
        }
        fn pause(&mut self, secs: u64, _stop: &AtomicBool) -> bool {
            self.clock.set(self.clock.get() + secs as i64);
            true
        }
    }

    const T0: i64 = 1_792_828_800; // 2026-10-24T08:00:00Z

    #[test]
    fn calibrate_worker_gets_the_offsets_and_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path());
        let mut r = req(SessionMode::Calibrate, 3600);
        r.offsets = vec![-300, -100, 0, 100];
        let Start::New(st) = open_or_resume(&p, &r, T0).unwrap() else {
            panic!("new")
        };
        let a = worker_args(&st, 1800, Some(4_000_000));
        let s = a.join(" ");
        assert_eq!(
            s,
            concat!(
                "live --purpose calibrate --duration-secs 1800 ",
                "--max-spend-lamports 4000000 --offsets -300,-100,0,100"
            )
        );
        // a dry-run session sends nothing live: no cap, no offsets
        let dir2 = tempfile::tempdir().unwrap();
        let mut r = req(SessionMode::Dryrun, 3600);
        r.offsets = vec![-300];
        let Start::New(d) = open_or_resume(&Paths::new(dir2.path()), &r, T0).unwrap() else {
            panic!("new")
        };
        assert_eq!(
            worker_args(&d, 60, None).join(" "),
            "dry-run --duration-secs 60"
        );
    }

    fn events(p: &Paths, id: &str) -> Vec<String> {
        read_events(p, id)
            .unwrap()
            .iter()
            .map(|e| e["event"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn crash_restart_resume_after_reboot_and_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path());
        let clock = Cell::new(T0);
        let Start::New(st) = open_or_resume(&p, &req(SessionMode::Dryrun, 4 * 3600), T0).unwrap()
        else {
            panic!("new")
        };
        assert_eq!(st.id, "20261024T075959Z");
        // first supervisor: worker crashes after 1 h, restarts, then the PC "loses power" at
        // 2 h (we simply stop calling it) with the heartbeat last written at 2 h.
        let mut w = Fake {
            clock: &clock,
            codes: vec![3],
            run_secs: 3600,
            runs: vec![],
            collects: 0,
            cap: None,
        };
        let stop = Stop::default();
        // emulate the power loss: run one crash cycle and one more run by hand
        let n0 = w.run(st.end_unix - clock.get()).unwrap();
        assert_eq!(n0, 3);
        event(
            &p,
            &st.id,
            clock.get(),
            "run_exit",
            json!({"run": 1, "exit_code": 3, "from": utc(T0), "to": utc(clock.get())}),
        )
        .unwrap();
        std::fs::write(p.heartbeat(&st.id), (T0 + 2 * 3600).to_string()).unwrap();
        // reboot: the operator runs the same command 30 min after the last heartbeat
        let back = T0 + 2 * 3600 + 1800;
        clock.set(back);
        let Start::Resumed(st2) =
            open_or_resume(&p, &req(SessionMode::Dryrun, 8 * 3600), back).unwrap()
        else {
            panic!("resume")
        };
        assert_eq!(
            st2.end_unix, st.end_unix,
            "same fixed interval, not a new 8 h"
        );
        assert_eq!(supervise(&p, &st2, &mut w, &stop).unwrap(), Outcome::Done);
        assert_eq!(w.runs[1], st.end_unix - back, "remaining time only");
        let ev = events(&p, &st.id);
        assert_eq!(&ev[..4], ["start", "run_exit", "down", "resume"]);
        let down = &read_events(&p, &st.id).unwrap()[2];
        assert_eq!(down["from"], "2026-10-24T10:00:00Z");
        assert_eq!(down["secs"], 1800);
        // coverage: hour 08 covered (first run), 09 not (crash, no worker logged), 10 half
        // (down until 10:30), 11 covered
        let md = finish(&p, dir.path(), &st2, st.end_unix).unwrap();
        let c: Value =
            serde_json::from_slice(&std::fs::read(p.dir(&st.id).join("coverage.json")).unwrap())
                .unwrap();
        let flags: Vec<&str> = c["hours"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["flag"].as_str().unwrap())
            .collect();
        let cov: Vec<i64> = c["hours"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["covered_secs"].as_i64().unwrap())
            .collect();
        assert_eq!(cov, vec![3600, 0, 1800, 3600]);
        assert_eq!(flags, vec!["OK", "GAP", "OK", "OK"]);
        assert_eq!(c["hours"][2]["blind_secs"]["down"], 1800);
        assert!(
            std::fs::read_to_string(md)
                .unwrap()
                .contains("| 2026-10-24T09:00:00Z | 60 | 0 |")
        );
        assert!(!p.open().exists(), "closed");
        assert!(p.dir(&st.id).join("state.json").exists());
    }

    #[test]
    fn worker_crash_is_restarted_after_a_logged_pause() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path());
        let clock = Cell::new(T0);
        let Start::New(st) = open_or_resume(&p, &req(SessionMode::Dryrun, 3600), T0).unwrap()
        else {
            panic!()
        };
        let mut w = Fake {
            clock: &clock,
            codes: vec![101],
            run_secs: 600,
            runs: vec![],
            collects: 0,
            cap: None,
        };
        assert_eq!(
            supervise(&p, &st, &mut w, &Stop::default()).unwrap(),
            Outcome::Done
        );
        assert_eq!(w.runs, vec![3600, 2970, 2370, 1770, 1170, 570]);
        let ev = read_events(&p, &st.id).unwrap();
        let down = ev.iter().find(|e| e["event"] == "worker_down").unwrap();
        assert_eq!(down["secs"], 30);
        assert_eq!(down["from"], "2026-10-24T08:10:00Z");
    }

    #[test]
    fn live_sessions_never_resume_and_restart_after_collect() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path());
        let clock = Cell::new(T0);
        let Start::New(st) = open_or_resume(&p, &req(SessionMode::Calibrate, 3600), T0).unwrap()
        else {
            panic!()
        };
        let mut w = Fake {
            clock: &clock,
            codes: vec![1],
            run_secs: 3000,
            runs: vec![],
            collects: 0,
            cap: Some(500),
        };
        supervise(&p, &st, &mut w, &Stop::default()).unwrap();
        assert_eq!(
            w.collects, 1,
            "a crashed live worker is followed by collect"
        );
        // the supervisor died before finishing: a restart must refuse to resume
        let err = open_or_resume(&p, &req(SessionMode::Calibrate, 3600), T0 + 7200).unwrap_err();
        assert!(err.to_string().contains("never resumed"), "{err}");
        assert!(open_or_resume(&p, &req(SessionMode::Dryrun, 3600), T0 + 7200).is_err());
        assert_eq!(
            reset(&p, T0 + 7200).unwrap().as_deref(),
            Some(st.id.as_str())
        );
        assert!(matches!(
            open_or_resume(&p, &req(SessionMode::Dryrun, 3600), T0 + 7300).unwrap(),
            Start::New(_)
        ));
    }

    #[test]
    fn cap_reached_stops_a_live_session_and_stop_flag_is_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path());
        let clock = Cell::new(T0);
        let Start::New(st) = open_or_resume(&p, &req(SessionMode::Measure, 3600), T0).unwrap()
        else {
            panic!()
        };
        let mut w = Fake {
            clock: &clock,
            codes: vec![],
            run_secs: 60,
            runs: vec![],
            collects: 0,
            cap: Some(0),
        };
        assert_eq!(
            supervise(&p, &st, &mut w, &Stop::default()).unwrap(),
            Outcome::Done
        );
        assert!(w.runs.is_empty());
        assert!(events(&p, &st.id).contains(&"cap_reached".to_string()));
        let stop = Stop::default();
        stop.request("console window closed");
        stop.request("Ctrl+C"); // the first reason wins
        w.cap = None;
        assert_eq!(supervise(&p, &st, &mut w, &stop).unwrap(), Outcome::Stopped);
        let last = read_events(&p, &st.id).unwrap().pop().unwrap();
        assert_eq!(last["event"], "stopped");
        assert_eq!(last["detail"], "console window closed");
    }

    /// A supervisor killed without notice (sign-out, power loss): the next start records one
    /// `abnormal_end` with the last heartbeat before anything else, and only once.
    #[test]
    fn dead_session_gets_one_abnormal_end_with_its_last_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path());
        let Start::New(st) = open_or_resume(&p, &req(SessionMode::Dryrun, 8 * 3600), T0).unwrap()
        else {
            panic!("new")
        };
        event(&p, &st.id, T0 + 1, "run_start", json!({"run": 1})).unwrap();
        let hb = T0 + 2_551; // last heartbeat 42 min in, then the process is gone
        std::fs::write(p.heartbeat(&st.id), hb.to_string()).unwrap();
        // still within DOWN_AFTER_SECS: it may be running, so nothing is written
        assert!(record_abnormal_end(&p, hb + 60).unwrap().is_none());
        assert_eq!(
            maybe_running(&p, hb + 60).unwrap(),
            Some((st.id.clone(), 60))
        );
        let later = hb + 11 * 3600;
        assert_eq!(maybe_running(&p, later).unwrap(), None);
        let ev = record_abnormal_end(&p, later).unwrap().unwrap();
        assert_eq!(ev["last_heartbeat"], utc(hb));
        assert_eq!(ev["last_event"], "run_start");
        assert!(
            record_abnormal_end(&p, later + 5).unwrap().is_none(),
            "recorded once"
        );
        let all = read_events(&p, &st.id).unwrap();
        let last = all.last().unwrap();
        assert_eq!(last["event"], "abnormal_end");
        assert_eq!(last["ts"], utc(later));
        assert!(last["last_heartbeat"].as_str().unwrap().ends_with('Z'));
        // --reset afterwards closes it; the run data is not touched
        assert_eq!(reset(&p, later + 10).unwrap(), Some(st.id.clone()));
        assert!(p.dir(&st.id).join("state.json").exists() && !p.open().exists());
    }

    #[test]
    fn orderly_stop_is_not_abnormal() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path());
        let Start::New(st) = open_or_resume(&p, &req(SessionMode::Dryrun, 3600), T0).unwrap()
        else {
            panic!("new")
        };
        event(&p, &st.id, T0 + 5, "stopped", json!({"detail": "Ctrl+C"})).unwrap();
        std::fs::write(p.heartbeat(&st.id), (T0 + 5).to_string()).unwrap();
        assert!(record_abnormal_end(&p, T0 + 5_000).unwrap().is_none());
        assert_eq!(
            maybe_running(&p, T0 + 10).unwrap(),
            None,
            "stopped: not running"
        );
        // no heartbeat at all (killed in its first seconds): still recorded
        let dir2 = tempfile::tempdir().unwrap();
        let p2 = Paths::new(dir2.path());
        let Start::New(st2) = open_or_resume(&p2, &req(SessionMode::Dryrun, 3600), T0).unwrap()
        else {
            panic!("new")
        };
        let ev = record_abnormal_end(&p2, T0 + 100).unwrap().unwrap();
        assert!(ev["last_heartbeat"].is_null());
        assert_eq!(ev["last_event"], "start");
        assert_eq!(read_events(&p2, &st2.id).unwrap().len(), 2);
    }

    #[test]
    fn session_limits_and_reserved_since() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path());
        assert!(open_or_resume(&p, &req(SessionMode::Dryrun, 11 * 3600), T0).is_err());
        let ledger = dir.path().join("ledger.jsonl");
        std::fs::write(
            &ledger,
            concat!(
                r#"{"kind":"reserve","amount":100,"ts":"2026-10-24T07:59:59Z"}"#,
                "\n",
                r#"{"kind":"reserve","amount":200,"ts":"2026-10-24T08:00:00Z"}"#,
                "\n",
                r#"{"kind":"landed","fee":50,"ts":"2026-10-24T08:00:01Z"}"#,
                "\n",
                r#"{"kind":"reserve","amount":300,"ts":"2026-10-24T09:00:00.5Z"}"#,
                "\n",
                r#"{"kind":"reserve","amo"#
            ),
        )
        .unwrap();
        assert_eq!(reserved_since(&ledger, T0).unwrap(), 500);
    }
}
