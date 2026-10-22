//! Token-bucket rate limiter shared by every clone of an `Rpc`.
//!
//! The provider limit (Helius free plan: 10 req/s) is enforced per second, so the bucket
//! must bound the count in *any* 1 s interval, not only the long-run rate. A bucket with
//! rate r and capacity c admits at most c + r requests in any 1 s interval. We run at
//! r = [`MAX_RPC_PER_SEC`] = 8 with c = 2, so the worst second is 10 and the sustained rate
//! is 8. Retries after a 429 take a token like any other request.
//!
//! Two implementations with the same bound:
//! - [`TokenBucket`]: in-process (tests, localnet).
//! - [`FileBucket`]: shared by every process on the machine through `data/.ratelimit/`
//!   (every `sender` subcommand: live, dry-run, collect, select-validators, ...; and
//!   `scripts/verify_facts.py`). It is a GCRA, the "virtual scheduling" form of the token
//!   bucket: one state value, the theoretical arrival time `tat_ns`, kept in `<name>.gcra`
//!   and updated under an exclusive OS lock on `<name>.lock`. Each caller reserves its slot
//!   under the lock, releases it, then sleeps until the slot. With emission interval
//!   T = 1/rate and tolerance tau = (burst - 1) * T, at most burst + rate requests start in
//!   any 1 s interval, across all processes together. The Python twin
//!   (`scripts/ratelimit.py`) uses the same files, format and arithmetic.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Upper bound for the general RPC bucket (the CLI value can only lower it).
pub const MAX_RPC_PER_SEC: f64 = 8.0;
pub const RPC_BURST: f64 = 2.0;
/// Helius free plan: sendTransaction is limited to 1/s on its own.
pub const MAX_SEND_TX_PER_SEC: f64 = 1.0;

struct State {
    tokens: f64,
    last: Instant,
}

#[derive(Clone)]
pub struct TokenBucket {
    rate: f64,
    capacity: f64,
    state: Arc<Mutex<State>>,
}

impl TokenBucket {
    pub fn new(rate_per_sec: f64, capacity: f64) -> Self {
        assert!(rate_per_sec > 0.0 && capacity >= 1.0);
        Self {
            rate: rate_per_sec,
            capacity,
            state: Arc::new(Mutex::new(State {
                tokens: capacity,
                last: Instant::now(),
            })),
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Wait for one token. The mutex is held across the sleep, so waiters are served in
    /// arrival order (tokio's Mutex is fair) and none can overtake.
    pub async fn acquire(&self) {
        let mut s = self.state.lock().await;
        let now = Instant::now();
        s.tokens =
            (s.tokens + now.duration_since(s.last).as_secs_f64() * self.rate).min(self.capacity);
        s.last = now;
        if s.tokens < 1.0 {
            let wait = Duration::from_secs_f64((1.0 - s.tokens) / self.rate);
            tokio::time::sleep(wait).await;
            s.tokens = 1.0;
            s.last = now + wait;
        }
        s.tokens -= 1.0;
    }
}

/// File names of the shared buckets (also used by `scripts/ratelimit.py`).
pub const SHARED_RPC: &str = "rpc";
pub const SHARED_SEND_TX: &str = "send_tx";
/// A stored tat further than this in the future is treated as a wall-clock jump.
const MAX_AHEAD_NS: i64 = 30_000_000_000;

fn unix_ns_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Cross-process GCRA bucket (see module docs).
#[derive(Clone, Debug)]
pub struct FileBucket {
    dir: PathBuf,
    name: String,
    rate: f64,
    interval_ns: i64,
    tau_ns: i64,
}

impl FileBucket {
    pub fn new(dir: &Path, name: &str, rate_per_sec: f64, burst: f64) -> Self {
        assert!(rate_per_sec > 0.0 && burst >= 1.0);
        let interval_ns = (1e9 / rate_per_sec).ceil() as i64;
        Self {
            dir: dir.to_path_buf(),
            name: name.to_string(),
            rate: rate_per_sec,
            interval_ns,
            tau_ns: ((burst - 1.0) * interval_ns as f64) as i64,
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Reserve the next slot; returns its UNIX time in ns (now or later). Blocking I/O.
    pub fn reserve(&self) -> std::io::Result<i64> {
        std::fs::create_dir_all(&self.dir)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(format!("{}.lock", self.name)))?;
        lock.lock()?;
        let state = self.dir.join(format!("{}.gcra", self.name));
        let stored = std::fs::read_to_string(&state)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v["tat_ns"].as_i64())
            .unwrap_or(0);
        let now = unix_ns_i64();
        let mut tat = stored.max(now);
        if tat - now > MAX_AHEAD_NS {
            tat = now; // the wall clock jumped backwards
        }
        let at = now.max(tat - self.tau_ns);
        let new_tat = tat + self.interval_ns;
        std::fs::write(&state, format!("{{\"tat_ns\": {new_tat}}}\n"))?;
        lock.unlock()?;
        Ok(at)
    }

    /// Wait for a slot. Returns the reserved UNIX time in ns.
    pub async fn acquire(&self) -> std::io::Result<i64> {
        let me = self.clone();
        let at = tokio::task::spawn_blocking(move || me.reserve())
            .await
            .map_err(std::io::Error::other)??;
        let wait = at - unix_ns_i64();
        if wait > 0 {
            tokio::time::sleep(Duration::from_nanos(wait as u64)).await;
        }
        Ok(at)
    }
}

/// The limiter an `Rpc` uses: in-process or shared across processes.
#[derive(Clone)]
pub enum Limiter {
    Local(TokenBucket),
    Shared(FileBucket),
}

impl Limiter {
    pub fn rate(&self) -> f64 {
        match self {
            Limiter::Local(b) => b.rate(),
            Limiter::Shared(b) => b.rate(),
        }
    }

    pub async fn acquire(&self) {
        match self {
            Limiter::Local(b) => b.acquire().await,
            Limiter::Shared(b) => {
                if let Err(e) = b.acquire().await {
                    // Never run unlimited: fall back to waiting one full interval.
                    tracing::warn!(error = %e, "shared rate limiter failed; pacing locally");
                    tokio::time::sleep(Duration::from_secs_f64(1.0 / b.rate())).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Max number of events in any half-open interval of length `w`.
    fn max_in_window(ts: &[Instant], w: Duration) -> usize {
        let mut best = 0;
        let mut j = 0;
        for i in 0..ts.len() {
            while ts[i].duration_since(ts[j]) >= w {
                j += 1;
            }
            best = best.max(i - j + 1);
        }
        best
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_callers_never_exceed_rate_plus_burst() {
        // Scaled 10x so the test takes ~0.4 s: 80/s with burst 2 => <= 10 per 100 ms.
        let b = TokenBucket::new(80.0, 2.0);
        let ts = Arc::new(std::sync::Mutex::new(vec![]));
        let start = Instant::now();
        let tasks: Vec<_> = (0..4)
            .map(|_| {
                let (b, ts) = (b.clone(), ts.clone());
                tokio::spawn(async move {
                    for _ in 0..9 {
                        b.acquire().await;
                        ts.lock().unwrap().push(Instant::now());
                    }
                })
            })
            .collect();
        for t in tasks {
            t.await.unwrap();
        }
        let mut ts = ts.lock().unwrap().clone();
        ts.sort();
        assert_eq!(ts.len(), 36);
        let w = max_in_window(&ts, Duration::from_millis(100));
        assert!(w <= 10, "{w} in 100 ms");
        // (36 - 2) tokens at 80/s take at least 425 ms.
        assert!(
            start.elapsed() >= Duration::from_millis(420),
            "{:?}",
            start.elapsed()
        );
    }

    /// Max number of reserved times in any half-open interval of length `w_ns`.
    fn max_in_window_ns(ts: &[i64], w_ns: i64) -> usize {
        let mut best = 0;
        let mut j = 0;
        for i in 0..ts.len() {
            while ts[i] - ts[j] >= w_ns {
                j += 1;
            }
            best = best.max(i - j + 1);
        }
        best
    }

    #[test]
    fn shared_bucket_bounds_all_handles_together() {
        // 4 threads, each with its own FileBucket (own file handles = what separate
        // processes get), 10 requests each. Scaled 10x: 80/s, burst 2 => <= 10 per 100 ms.
        let dir = tempfile::tempdir().unwrap();
        let start = unix_ns_i64();
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let b = FileBucket::new(dir.path(), SHARED_RPC, 80.0, 2.0);
                std::thread::spawn(move || {
                    (0..10).map(|_| b.reserve().unwrap()).collect::<Vec<_>>()
                })
            })
            .collect();
        let mut ts: Vec<i64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        ts.sort();
        assert_eq!(ts.len(), 40);
        let w = max_in_window_ns(&ts, 100_000_000);
        assert!(w <= 10, "{w} reservations in 100 ms");
        // 40 slots at 12.5 ms with 1 slot of tolerance span >= 38 intervals.
        assert!(ts[39] - ts[0] >= 38 * 12_500_000, "{}", ts[39] - ts[0]);
        assert!(ts[0] >= start - 1_000_000);
    }

    #[test]
    fn shared_bucket_is_shared_across_processes() {
        // Child process: reserve 10 slots from the shared directory and print them.
        if let Ok(d) = std::env::var("SLWLAB_RATELIMIT_CHILD") {
            let b = FileBucket::new(Path::new(&d), SHARED_RPC, 80.0, 2.0);
            let ts: Vec<String> = (0..10).map(|_| b.reserve().unwrap().to_string()).collect();
            println!("SLOTS {}", ts.join(","));
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let children: Vec<_> = (0..3)
            .map(|_| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "ratelimit::tests::shared_bucket_is_shared_across_processes",
                        "--nocapture",
                    ])
                    .env("SLWLAB_RATELIMIT_CHILD", dir.path())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        let mut ts = vec![];
        for c in children {
            let out = String::from_utf8(c.wait_with_output().unwrap().stdout).unwrap();
            let line = out
                .lines()
                .find_map(|l| l.strip_prefix("SLOTS "))
                .expect("child output");
            ts.extend(line.split(',').map(|v| v.parse::<i64>().unwrap()));
        }
        ts.sort();
        assert_eq!(ts.len(), 30);
        let w = max_in_window_ns(&ts, 100_000_000);
        assert!(w <= 10, "{w} reservations in 100 ms across 3 processes");
    }

    #[test]
    fn backwards_clock_jump_does_not_stall() {
        let dir = tempfile::tempdir().unwrap();
        let far = unix_ns_i64() + 3_600_000_000_000;
        std::fs::write(
            dir.path().join("rpc.gcra"),
            format!("{{\"tat_ns\": {far}}}"),
        )
        .unwrap();
        let b = FileBucket::new(dir.path(), SHARED_RPC, 8.0, 2.0);
        assert!(b.reserve().unwrap() - unix_ns_i64() < 1_000_000_000);
    }
}
