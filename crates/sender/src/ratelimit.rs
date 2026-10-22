//! Token-bucket rate limiter shared by every clone of an `Rpc`.
//!
//! The provider limit (Helius free plan: 10 req/s) is enforced per second, so the bucket
//! must bound the count in *any* 1 s interval, not only the long-run rate. A bucket with
//! rate r and capacity c admits at most c + r requests in any 1 s interval. We run at
//! r = [`MAX_RPC_PER_SEC`] = 8 with c = 2, so the worst second is 10 and the sustained rate
//! is 8. Retries after a 429 take a token like any other request.

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
}
