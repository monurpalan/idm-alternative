//! Speed limits: a token bucket shared by every connection that should obey it.
//!
//! A [`Limiter`] is a cheap handle; clones share one bucket. The app keeps one global
//! limiter for all downloads and one per download, and passes both in [`Options::limiters`](crate::Options::limiters)
//! . Rates can change while downloads run.
//!
//! Each connection *reserves* the bytes it just received and sleeps until the bucket has
//! paid for them. Reservations queue up as debt, so the total rate stays exact however many
//! connections share the bucket, and a connection that waits stops reading its socket, which
//! lets TCP slow the server down instead of buffering.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Credit a bucket may accumulate while idle, in seconds of its rate: enough to absorb a
/// connection pausing to write to disk, small enough that the burst after it stays within
/// a few percent over any few seconds. The clock only starts at the first reservation, so a
/// limiter created well before its download starts has no credit saved up.
const BURST: f64 = 0.25;

#[derive(Clone, Default)]
pub struct Limiter {
    inner: Arc<Mutex<Bucket>>,
}

#[derive(Default)]
struct Bucket {
    /// Bytes per second; `None` = unlimited.
    rate: Option<u64>,
    /// Credit in bytes as of `last`; negative while reservations are waiting to be paid.
    tokens: f64,
    /// `None` until the first reservation.
    last: Option<Instant>,
}

impl Bucket {
    /// Credit at `now`, without recording anything.
    fn credit(&self, rate: u64, now: Instant) -> f64 {
        let Some(last) = self.last else {
            return self.tokens;
        };
        let cap = (rate as f64 * BURST).max(16.0 * 1024.0);
        (self.tokens + now.saturating_duration_since(last).as_secs_f64() * rate as f64).min(cap)
    }

    fn refill(&mut self, rate: u64, now: Instant) {
        self.tokens = self.credit(rate, now);
        self.last = Some(now);
    }
}

impl std::fmt::Debug for Limiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Limiter")
            .field("rate", &self.rate())
            .finish()
    }
}

impl Limiter {
    /// `rate` in bytes per second; `None` for unlimited.
    pub fn new(rate: Option<u64>) -> Self {
        let l = Self::default();
        l.set_rate(rate);
        l
    }

    pub fn rate(&self) -> Option<u64> {
        self.bucket().rate
    }

    /// Changes the rate; takes effect for the next reservation. Outstanding debt is kept,
    /// so lowering the limit cannot be dodged by connections that reserved just before.
    pub fn set_rate(&self, rate: Option<u64>) {
        let mut b = self.bucket();
        let rate = rate.filter(|r| *r > 0);
        match b.rate {
            Some(old) if b.last.is_some() => b.refill(old, Instant::now()),
            // Unlimited until now: start afresh with the next reservation.
            _ => {
                b.tokens = 0.0;
                b.last = None;
            }
        }
        b.rate = rate;
    }

    /// Whether downloads are asking for at least what the limit allows. Then the limit, not
    /// the server, sets the speed, and opening or replacing connections gains nothing — it
    /// only abandons responses whose already buffered data is wasted.
    ///
    /// Demand at the limit keeps credit around zero, with short spikes whenever every
    /// connection happens to be reading at once; only credit piling up past half the burst
    /// allowance means the connections cannot use the rate.
    pub fn is_saturated(&self) -> bool {
        let b = self.bucket();
        let Some(rate) = b.rate else {
            return false;
        };
        b.credit(rate, Instant::now()) < rate as f64 * BURST / 2.0
    }

    /// How long a connection must wait after receiving `bytes`.
    fn reserve(&self, bytes: usize, now: Instant) -> Duration {
        let mut b = self.bucket();
        let Some(rate) = b.rate else {
            return Duration::ZERO;
        };
        b.refill(rate, now);
        b.tokens -= bytes as f64;
        if b.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-b.tokens / rate as f64)
        }
    }

    /// Pays for `bytes` that were just received, sleeping as long as the limit requires.
    pub async fn acquire(&self, bytes: usize) {
        let wait = self.reserve(bytes, Instant::now());
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    fn bucket(&self) -> std::sync::MutexGuard<'_, Bucket> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Within a millisecond: `Limiter::new` stamps its own clock just before the test does.
    fn about(d: Duration, ms: u64) -> bool {
        d.as_secs_f64().mul_add(1000.0, -(ms as f64)).abs() < 1.0
    }

    #[test]
    fn unlimited_never_waits() {
        let l = Limiter::new(None);
        assert_eq!(l.reserve(1 << 30, Instant::now()), Duration::ZERO);
    }

    #[test]
    fn reservations_queue_up_as_debt() {
        let l = Limiter::new(Some(1000));
        let now = Instant::now();
        // Starts without credit: no burst at the beginning of a download.
        assert!(about(l.reserve(500, now), 500));
        assert!(about(l.reserve(500, now), 1000));
        // One second later all the debt is paid.
        assert!(about(l.reserve(0, now + Duration::from_secs(1)), 0));
    }

    #[test]
    fn idle_credit_is_capped() {
        let l = Limiter::new(Some(1 << 20));
        let now = Instant::now();
        l.reserve(0, now);
        // An idle minute buys only a quarter second of burst.
        let later = now + Duration::from_secs(60);
        assert_eq!(l.reserve(256 << 10, later), Duration::ZERO);
        assert!(l.reserve(256 << 10, later) >= Duration::from_millis(240));
    }

    #[test]
    fn no_credit_before_the_first_reservation() {
        let l = Limiter::new(Some(1000));
        // Created long before the download starts: still no burst.
        let later = Instant::now() + Duration::from_secs(60);
        assert!(about(l.reserve(500, later), 500));
    }

    #[test]
    fn saturation_follows_demand() {
        let l = Limiter::new(Some(1 << 20));
        assert!(!Limiter::new(None).is_saturated());
        // Reservations waiting: saturated.
        l.reserve(64 << 10, Instant::now());
        assert!(l.is_saturated());
        // Credit piling up unused: not saturated.
        std::thread::sleep(Duration::from_millis(300));
        assert!(!l.is_saturated());
    }

    #[test]
    fn rate_change_applies_to_next_reservation() {
        let l = Limiter::new(Some(1000));
        let now = Instant::now();
        l.set_rate(Some(10_000));
        assert!(l.reserve(1000, now) <= Duration::from_millis(101));
        l.set_rate(None);
        assert_eq!(l.reserve(1 << 20, now), Duration::ZERO);
    }
}
