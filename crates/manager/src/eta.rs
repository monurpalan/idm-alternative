//! Time left as a range: the speed is averaged over seconds (a fast average for where it is
//! heading, a slow one for where it has been, and how much it varies), a site's speed in
//! earlier downloads stands in while little is known, and the range is as wide as the speed
//! is unsteady, narrower the longer the rest takes (unsteadiness evens out over time).

/// Seconds of data before any estimate: before that it is "calculating".
const SETTLE: u32 = 3;
/// How fast the averages follow, per second.
const FAST: f64 = 0.5;
const SLOW: f64 = 0.15;
/// A site's earlier speed counts as this many seconds of this download's.
const PRIOR_WEIGHT: f64 = 3.0;
/// How long (s) unsteadiness takes to even out: the range of a long rest narrows by
/// `sqrt(EVEN_OUT / rest)`.
const EVEN_OUT: f64 = 5.0;
/// The narrowest range, around the middle: a speed is never known better than this.
const MIN_SPREAD: f64 = 0.04;
/// How fast the trend follows, per second.
const TREND: f64 = 0.1;
/// The longest a trend is carried on, seconds.
const HORIZON: f64 = 60.0;

/// Follows one running download's progress.
#[derive(Debug, Clone)]
pub struct Estimator {
    /// Where the current one-second bucket started: (seconds, bytes).
    bucket: Option<(f64, u64)>,
    fast: f64,
    slow: f64,
    /// Of the per-second speed around `slow`.
    variance: f64,
    /// `fast - slow`, itself averaged: steady when the speed keeps rising or falling, near
    /// nothing when it only jumps about.
    trend: f64,
    seconds: u32,
    prior: Option<f64>,
}

impl Estimator {
    /// `prior`: the site's speed in earlier downloads, bytes per second.
    pub fn new(prior: Option<f64>) -> Self {
        Self {
            bucket: None,
            fast: 0.0,
            slow: 0.0,
            variance: 0.0,
            trend: 0.0,
            seconds: 0,
            prior: prior.filter(|p| *p > 0.0),
        }
    }

    /// Bytes received so far (`received`) at `now` seconds (any fixed origin).
    pub fn update(&mut self, now: f64, received: u64) {
        let Some((start, from)) = self.bucket else {
            self.bucket = Some((now, received));
            return;
        };
        let elapsed = now - start;
        if elapsed < 1.0 {
            return;
        }
        let speed = received.saturating_sub(from) as f64 / elapsed;
        self.bucket = Some((now, received));
        // A pause of several seconds counts as that many slow seconds, not one.
        let n = (elapsed.round() as u32).clamp(1, 10);
        for _ in 0..n {
            self.sample(speed);
        }
    }

    fn sample(&mut self, speed: f64) {
        if self.seconds == 0 {
            self.fast = speed;
            self.slow = speed;
        } else {
            self.fast += FAST * (speed - self.fast);
            let d = speed - self.slow;
            self.slow += SLOW * d;
            self.variance = (1.0 - SLOW) * (self.variance + SLOW * d * d);
            self.trend += TREND * ((self.fast - self.slow) - self.trend);
        }
        self.seconds += 1;
    }

    /// The speed now, bytes per second: the slow average, leaning on the prior early on.
    pub fn speed(&self) -> Option<f64> {
        if self.seconds == 0 {
            return None;
        }
        let n = f64::from(self.seconds);
        Some(match self.prior {
            Some(p) if n < 30.0 => {
                let w = PRIOR_WEIGHT * (1.0 - n / 30.0);
                (p * w + self.slow * n) / (w + n)
            }
            _ => self.slow,
        })
    }

    /// Seconds left for `remaining` bytes, `(soonest, latest)`; `None` while calculating or
    /// while nothing arrives.
    pub fn eta(&self, remaining: u64) -> Option<(f64, f64)> {
        if remaining == 0 {
            return Some((0.0, 0.0));
        }
        if self.seconds < SETTLE {
            return None;
        }
        let base = self.speed()?;
        if base <= 1.0 {
            return None;
        }
        let rest = remaining as f64;
        let sd = self.variance.sqrt() * (EVEN_OUT / (rest / base).max(EVEN_OUT)).sqrt();
        // Where it is heading counts: a falling speed widens the late end, a rising one the
        // early end. The two averages lag by different times, so their difference is the
        // trend; carried on for half the rest (at most a minute: trends end), it is the
        // average speed still to come.
        let slope = self.trend / (1.0 / SLOW - 1.0 / FAST);
        let ahead = (self.fast + slope * (rest / base).min(HORIZON) / 2.0).max(base * 0.2);
        let high = (self.fast.max(base).max(ahead) + sd).max(base * (1.0 + MIN_SPREAD));
        let low = (self.fast.min(base).min(ahead) - sd)
            .min(base * (1.0 - MIN_SPREAD))
            .max(base * 0.2);
        Some((rest / high, rest / low))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a download of `size` bytes whose speed at second `t` is `speed(t)`, sampled four
    /// times a second as the manager does; returns, for every second after the first few,
    /// the estimate and the true time left.
    fn run(size: u64, speed: impl Fn(f64) -> f64, prior: Option<f64>) -> Vec<((f64, f64), f64)> {
        let mut e = Estimator::new(prior);
        let mut received = 0.0;
        let mut t = 0.0;
        let mut seen = Vec::new();
        while received < size as f64 {
            e.update(t, received as u64);
            if ((t * 4.0).round() as u64).is_multiple_of(4)
                && let Some(est) = e.eta(size - received as u64)
            {
                seen.push((est, t));
            }
            received += speed(t) * 0.25;
            t += 0.25;
        }
        seen.into_iter().map(|(est, at)| (est, t - at)).collect()
    }

    fn covered(seen: &[((f64, f64), f64)]) -> f64 {
        let hits = seen
            .iter()
            .filter(|((lo, hi), truth)| *lo <= *truth + 1.0 && *truth <= *hi + 1.0)
            .count();
        hits as f64 / seen.len() as f64
    }

    /// Total change of the shown middle, per second: how much it jumps around.
    fn jumpiness(mids: &[f64]) -> f64 {
        mids.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / mids.len() as f64
    }

    #[test]
    fn calculating_at_first_then_a_narrow_range_for_a_steady_speed() {
        let mut e = Estimator::new(None);
        e.update(0.0, 0);
        e.update(1.0, 1000);
        assert_eq!(e.eta(10_000), None, "calculating");
        let seen = run(100 << 20, |_| 1_048_576.0, None);
        assert!(covered(&seen) > 0.99);
        let ((lo, hi), truth) = seen[10];
        assert!((hi - lo) / truth < 0.1, "narrow: {lo}..{hi} for {truth}");
        assert_eq!(e.eta(0), Some((0.0, 0.0)));
    }

    #[test]
    fn a_jumpy_speed_gives_a_wider_range_that_covers_the_end_and_jumps_less() {
        // Between 0.5 and 1.5 MiB/s, changing every second and a half.
        let jumpy = |t: f64| {
            if ((t / 1.5) as u64).is_multiple_of(2) {
                524_288.0
            } else {
                1_572_864.0
            }
        };
        let seen = run(200 << 20, jumpy, None);
        assert!(covered(&seen) > 0.9, "covered {}", covered(&seen));
        // Compared with time left from the speed of the last second, as before.
        let mut naive = Vec::new();
        let mut received = 0.0;
        let mut t = 0.0;
        while received < (200 << 20) as f64 {
            let last = jumpy(t);
            naive.push(((200 << 20) as f64 - received) / last);
            received += last;
            t += 1.0;
        }
        let mids: Vec<f64> = seen.iter().map(|((lo, hi), _)| (lo + hi) / 2.0).collect();
        assert!(
            jumpiness(&mids) < jumpiness(&naive) / 3.0,
            "{} vs {}",
            jumpiness(&mids),
            jumpiness(&naive)
        );
    }

    #[test]
    fn a_falling_speed_is_followed() {
        // From 4 MiB/s down to 1 MiB/s over the first minute, then steady.
        let falling = |t: f64| 1_048_576.0 * (4.0 - 3.0 * (t / 60.0).min(1.0));
        let seen = run(300 << 20, falling, None);
        // Not in the first seconds: they cannot know it will fall. Once it is falling, mostly;
        // once it is steady again, always.
        assert!(
            covered(&seen[15..]) > 0.85,
            "covered {}",
            covered(&seen[15..])
        );
        let steady: Vec<_> = seen.iter().copied().filter(|(_, t)| *t < 120.0).collect();
        assert!(covered(&steady) > 0.98, "covered {}", covered(&steady));
    }

    #[test]
    fn a_sites_earlier_speed_stands_in_at_first() {
        let mut slow_start = Estimator::new(Some(2_000_000.0));
        let mut none = Estimator::new(None);
        for (t, r) in [(0.0, 0), (1.0, 200_000), (2.0, 400_000), (3.0, 600_000)] {
            slow_start.update(t, r);
            none.update(t, r);
        }
        // The first seconds were slow (a connection setting up); the site is usually fast.
        let (with, _) = slow_start.eta(20_000_000).expect("estimate");
        let (without, _) = none.eta(20_000_000).expect("estimate");
        assert!(with < without / 2.0, "{with} vs {without}");
        // It fades: after a long while only this download counts.
        for s in 4u32..100 {
            slow_start.update(f64::from(s), 200_000 * u64::from(s));
        }
        let speed = slow_start.speed().expect("speed");
        assert!((speed - 200_000.0).abs() < 10_000.0, "{speed}");
    }

    #[test]
    fn a_stall_is_not_an_estimate() {
        let mut e = Estimator::new(None);
        for t in 0..6 {
            e.update(f64::from(t), 0);
        }
        assert_eq!(e.eta(1000), None);
    }
}
