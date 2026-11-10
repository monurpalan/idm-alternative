//! Segment table and the splitting policy.
//!
//! The file is covered by segments `[start, end)`. Each active segment is owned by one
//! connection. When a connection is free it first picks up an unowned segment (a failed
//! one waiting for retry), otherwise it *steals* part of the segment that is expected to
//! finish last. The split point is chosen from both speeds so that victim and thief should
//! finish at about the same time — a crawling connection keeps only a sliver, which is what
//! removes the slow tail that fixed-size piece schedulers suffer from.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub type WorkerId = u64;

#[derive(Debug, Clone)]
pub struct Segment {
    pub start: u64,
    /// Exclusive; shrinks when another connection steals the rest.
    pub end: u64,
    /// Bytes received so far (written or still in the owner's buffer).
    pub received: u64,
    /// Bytes handed to the OS; only this much is safe to record as done.
    pub written: u64,
    pub owner: Option<WorkerId>,
    /// Bytes per second, smoothed.
    pub speed: f64,
    pub owned_since: Option<Instant>,
    /// Consecutive failures without progress.
    pub failures: u32,
    pub retry_at: Option<Instant>,
    sampled: u64,
    /// `written` when the current owner took over; tells whether an attempt made progress.
    written_at_claim: u64,
    /// Times a crawling owner of this segment was dropped.
    pub evictions: u32,
    /// When the owner's latest response arrived, and `received` then.
    answered: Option<(Instant, u64)>,
}

impl Segment {
    fn new(start: u64, end: u64) -> Self {
        Self {
            start,
            end,
            received: start,
            written: start,
            owner: None,
            speed: 0.0,
            owned_since: None,
            failures: 0,
            retry_at: None,
            sampled: start,
            written_at_claim: start,
            evictions: 0,
            answered: None,
        }
    }

    pub fn remaining(&self) -> u64 {
        self.end.saturating_sub(self.received)
    }

    pub fn is_done(&self) -> bool {
        self.written >= self.end
    }
}

/// Persisted form of a segment: only durable progress.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedSegment {
    pub start: u64,
    pub end: u64,
    pub written: u64,
    /// Bytes per second when it was saved: how much to download again when the last attempt
    /// did not stop cleanly (see [`crate::state::State::load`]).
    #[serde(default)]
    pub speed: u64,
}

#[derive(Debug, Clone)]
pub struct SplitPolicy {
    /// Do not create pieces smaller than this.
    pub min_piece: u64,
    /// Only steal from a segment expected to need longer than this.
    pub min_eta: Duration,
    /// How much the victim always keeps ahead of what it has received, so it never
    /// receives bytes past its new end.
    pub victim_reserve: Duration,
}

impl Default for SplitPolicy {
    fn default() -> Self {
        Self {
            min_piece: 256 << 10,
            min_eta: Duration::from_millis(400),
            victim_reserve: Duration::from_millis(300),
        }
    }
}

#[derive(Debug, Default)]
pub struct Table {
    pub segs: Vec<Segment>,
    /// Highest smoothed per-connection speed observed.
    best_speed: f64,
}

impl Table {
    pub fn new(size: u64) -> Self {
        Self {
            segs: vec![Segment::new(0, size)],
            best_speed: 0.0,
        }
    }

    pub fn from_saved(saved: &[SavedSegment]) -> Self {
        let segs = saved
            .iter()
            .map(|s| {
                let mut seg = Segment::new(s.start, s.end);
                seg.received = s.written;
                seg.written = s.written;
                seg.sampled = s.written;
                seg
            })
            .collect();
        Self {
            segs,
            best_speed: 0.0,
        }
    }

    pub fn saved(&self) -> Vec<SavedSegment> {
        self.segs
            .iter()
            .map(|s| SavedSegment {
                start: s.start,
                end: s.end,
                written: s.written,
                speed: s.speed as u64,
            })
            .collect()
    }

    pub fn received_total(&self) -> u64 {
        self.segs
            .iter()
            .map(|s| s.received.min(s.end) - s.start)
            .sum()
    }

    pub fn is_complete(&self) -> bool {
        self.segs.iter().all(Segment::is_done)
    }

    pub fn active(&self) -> usize {
        self.segs.iter().filter(|s| s.owner.is_some()).count()
    }

    /// Updates smoothed speeds; call at a steady interval.
    pub fn sample_speeds(&mut self, dt: Duration) {
        let dt = dt.as_secs_f64().max(1e-3);
        for s in &mut self.segs {
            let inst = (s.received - s.sampled) as f64 / dt;
            s.sampled = s.received;
            s.speed = if s.owner.is_none() {
                0.0
            } else if s.speed == 0.0 {
                inst
            } else {
                0.75 * s.speed + 0.25 * inst
            };
            self.best_speed = self.best_speed.max(s.speed);
        }
    }

    /// Median speed of the active connections that have been measured.
    pub fn typical_speed(&self) -> Option<f64> {
        let mut speeds: Vec<f64> = self
            .segs
            .iter()
            .filter(|s| s.owner.is_some() && s.speed > 0.0)
            .map(|s| s.speed)
            .collect();
        speeds.sort_by(f64::total_cmp);
        speeds.get(speeds.len() / 2).copied()
    }

    /// Active connections whose speed has not been measured yet: just started.
    pub fn unmeasured(&self) -> usize {
        self.segs
            .iter()
            .filter(|s| s.owner.is_some() && s.speed == 0.0)
            .count()
    }

    /// The fastest a single connection has gone in this download.
    pub fn best_speed(&self) -> f64 {
        self.best_speed
    }

    pub fn total_speed(&self) -> f64 {
        self.segs.iter().map(|s| s.speed).sum()
    }

    /// Gives segment `idx` to worker `id`.
    pub fn claim(&mut self, idx: usize, id: WorkerId, now: Instant) -> usize {
        let s = &mut self.segs[idx];
        s.owner = Some(id);
        s.owned_since = Some(now);
        s.answered = None;
        s.retry_at = None;
        s.speed = 0.0;
        s.sampled = s.received;
        s.written_at_claim = s.written;
        idx
    }

    /// Gives worker `id` something to do: an unowned unfinished segment whose retry delay
    /// has passed, or (with `steal`) a piece stolen from the slowest-to-finish segment.
    pub fn assign(
        &mut self,
        id: WorkerId,
        now: Instant,
        policy: &SplitPolicy,
        steal: bool,
    ) -> Option<usize> {
        if let Some(idx) = self
            .segs
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.owner.is_none() && !s.is_done() && s.retry_at.is_none_or(|t| t <= now)
            })
            .min_by_key(|(_, s)| s.start)
            .map(|(i, _)| i)
        {
            return Some(self.claim(idx, id, now));
        }
        if !steal {
            return None;
        }
        self.steal(id, now, policy)
    }

    fn steal(&mut self, id: WorkerId, now: Instant, policy: &SplitPolicy) -> Option<usize> {
        // A segment whose speed is not known yet is assumed to run at the best speed seen
        // (or 1 B/s before anything is known), so among unknowns the largest is split first.
        let assumed = self.best_speed.max(1.0);
        let eta = |s: &Segment| -> f64 {
            s.remaining() as f64 / if s.speed > 0.0 { s.speed } else { assumed }
        };
        let (victim, _) = self
            .segs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.owner.is_some() && s.remaining() >= 2 * policy.min_piece)
            .map(|(i, s)| (i, eta(s)))
            .max_by(|a, b| {
                a.1.total_cmp(&b.1)
                    .then(self.segs[b.0].start.cmp(&self.segs[a.0].start))
            })?;

        let v = &self.segs[victim];
        if eta(v) < policy.min_eta.as_secs_f64() {
            return None;
        }
        // What the thief is expected to manage: the median of the other active
        // connections, or the best per-connection speed seen so far.
        let mut others: Vec<f64> = self
            .segs
            .iter()
            .enumerate()
            .filter(|(i, s)| *i != victim && s.owner.is_some() && s.speed > 0.0)
            .map(|(_, s)| s.speed)
            .collect();
        others.sort_by(f64::total_cmp);
        let typical = others
            .get(others.len() / 2)
            .copied()
            .or((self.best_speed > 0.0).then_some(self.best_speed));
        let remaining = v.remaining();
        // With both speeds known, split so both finish together; otherwise halve.
        let keep = match (v.speed > 0.0, typical) {
            (true, Some(u)) if u > 0.0 => (remaining as f64 * v.speed / (v.speed + u)) as u64,
            _ => remaining / 2,
        };
        // The victim keeps at least what may already be in flight to it; a slow victim
        // ends up with only that, and the controller evicts it if even that drags on.
        let reserve = ((v.speed * policy.victim_reserve.as_secs_f64()) as u64).max(64 << 10);
        let keep = keep.max(reserve);
        if remaining.saturating_sub(keep) < policy.min_piece {
            return None;
        }
        let split = v.received + keep;
        let old_end = v.end;
        self.segs[victim].end = split;

        self.segs.push(Segment::new(split, old_end));
        let idx = self.segs.len() - 1;
        Some(self.claim(idx, id, now))
    }

    /// Releases a segment after its owner stopped. A failed attempt that made progress is
    /// retried immediately; repeated failures without progress back off exponentially.
    pub fn release(&mut self, idx: usize, failed: bool, now: Instant) {
        let s = &mut self.segs[idx];
        s.owner = None;
        s.owned_since = None;
        s.answered = None;
        s.speed = 0.0;
        // Anything received but not written died with the worker's buffer.
        s.received = s.written;
        s.sampled = s.written;
        if !failed {
            return;
        }
        if s.written > s.written_at_claim {
            s.failures = 0;
            s.retry_at = None;
        } else {
            s.failures += 1;
            let backoff = Duration::from_millis(500) * 2u32.saturating_pow(s.failures.min(5) - 1);
            s.retry_at = Some(now + backoff.min(Duration::from_secs(8)));
        }
    }

    /// The owner of segment `idx` got a response: from now on, silence counts.
    pub fn answered(&mut self, idx: usize, now: Instant) {
        let s = &mut self.segs[idx];
        s.answered = Some((now, s.received));
    }

    /// The owner of segment `idx` sent a new request: until it is answered, the segment is
    /// waiting, not silent.
    pub fn asking(&mut self, idx: usize) {
        self.segs[idx].answered = None;
    }

    /// Whether the owner of segment `idx` is still waiting for an answer while another
    /// connection of the download got one: the server is holding that request back.
    pub fn held_back(&self, idx: usize) -> bool {
        self.segs[idx].owner.is_some()
            && self.segs[idx].answered.is_none()
            && self
                .segs
                .iter()
                .enumerate()
                .any(|(i, s)| i != idx && s.owner.is_some() && s.answered.is_some())
    }

    /// Segment `idx` is not tried again before `until` (a busy server's `Retry-After`).
    pub fn hold(&mut self, idx: usize, until: Instant) {
        let s = &mut self.segs[idx];
        s.retry_at = Some(s.retry_at.map_or(until, |r| r.max(until)));
    }

    /// A connection that has crawled for a while at a small fraction of the typical speed,
    /// and would take noticeably longer to finish its segment than a fresh connection. The
    /// controller drops it so a fresh connection can take the range.
    ///
    /// "Typical" is the median of the other active connections, or the best speed seen when
    /// the crawler is the last one left — exactly when it holds up the whole download.
    ///
    /// A connection whose response arrived and then nothing for five seconds, in a download
    /// where others have received data, goes first: it has no speed to compare, and would
    /// otherwise wait for the read timeout. That includes the last one left, holding a sliver
    /// after the others took the rest of its range. One still waiting for its response is
    /// left alone: some servers take that long to answer every request.
    pub fn laggard(&self, now: Instant) -> Option<usize> {
        /// What dropping a connection and opening a new one is assumed to cost.
        const RECONNECT: f64 = 0.25;
        /// How long an answered connection may get no data at all while others download.
        const SILENT: Duration = Duration::from_secs(5);
        let age = |s: &Segment| s.owned_since.map_or(Duration::ZERO, |t| now - t);
        // Some connection of this download has received data, so the network works.
        if self.best_speed > 0.0 {
            let silent = self.segs.iter().position(|s| {
                s.owner.is_some()
                    && s.evictions < 3
                    && s.remaining() > 0
                    && s.answered
                        .is_some_and(|(at, then)| s.received == then && now - at >= SILENT)
            });
            if silent.is_some() {
                return silent;
            }
        }
        let mut speeds: Vec<f64> = self
            .segs
            .iter()
            .filter(|s| s.owner.is_some() && s.speed > 0.0 && age(s) >= Duration::from_secs(1))
            .map(|s| s.speed)
            .collect();
        speeds.sort_by(f64::total_cmp);
        let typical = match speeds.len() {
            0 => return None,
            1 => self.best_speed,
            n => speeds[n / 2],
        };
        self.segs
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let remaining = s.remaining() as f64;
                s.owner.is_some()
                    && s.evictions < 3
                    && age(s) >= Duration::from_secs(2)
                    && s.speed > 0.0
                    && s.speed < 0.15 * typical
                    && remaining / s.speed > remaining / typical + RECONNECT
            })
            .max_by_key(|(_, s)| s.remaining())
            .map(|(i, _)| i)
    }

    /// Single-connection mode cannot resume mid-file: a failed attempt starts over.
    pub fn rewind(&mut self, idx: usize) {
        let s = &mut self.segs[idx];
        s.received = s.start;
        s.written = s.start;
        s.sampled = s.start;
        s.written_at_claim = s.start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1 << 20;

    #[test]
    fn halves_when_speed_unknown() {
        let now = Instant::now();
        let mut t = Table::new(100 * MB);
        let policy = SplitPolicy::default();
        assert_eq!(t.assign(1, now, &policy, true), Some(0));
        assert_eq!(t.assign(2, now, &policy, true), Some(1));
        assert_eq!((t.segs[0].start, t.segs[0].end), (0, 50 * MB));
        assert_eq!((t.segs[1].start, t.segs[1].end), (50 * MB, 100 * MB));
        // Without stealing, only unowned segments are handed out.
        assert_eq!(t.assign(3, now, &policy, false), None);
        assert_eq!(t.segs.len(), 2);
    }

    #[test]
    fn slow_victim_keeps_only_a_sliver() {
        let now = Instant::now();
        let policy = SplitPolicy::default();
        let mut t = Table::new(100 * MB);
        t.assign(1, now, &policy, true);
        t.assign(2, now, &policy, true);
        // Segment 0 crawls at 64 KiB/s; segment 1 is healthy at 8 MiB/s.
        t.segs[0].speed = 64.0 * 1024.0;
        t.segs[0].received = 10 * MB;
        t.segs[1].speed = 8.0 * MB as f64;
        t.segs[1].received = 60 * MB;
        assert_eq!(t.assign(3, now, &policy, true), Some(2));
        let stolen = &t.segs[2];
        assert_eq!(stolen.end, 50 * MB);
        assert_eq!(t.segs[0].end, stolen.start);
        assert!(
            stolen.start < 11 * MB,
            "victim kept {} bytes",
            stolen.start - 10 * MB
        );
    }

    #[test]
    fn last_crawler_is_dropped_even_with_a_small_remainder() {
        let start = Instant::now();
        let now = start + Duration::from_secs(3);
        let policy = SplitPolicy::default();
        let mut t = Table::new(100 * MB);
        t.assign(1, start, &policy, true);
        t.segs[0].received = 100 * MB - (64 << 10);
        t.best_speed = 8.0 * MB as f64;
        // Alone, crawling at 64 KiB/s with 64 KiB left: one second for it, ~0 for a new one.
        t.segs[0].speed = 64.0 * 1024.0;
        assert_eq!(t.laggard(now), Some(0));
        // At a healthy speed it is left alone.
        t.segs[0].speed = 4.0 * MB as f64;
        assert_eq!(t.laggard(now), None);
    }

    #[test]
    fn silent_connection_is_dropped_while_others_download() {
        let start = Instant::now();
        let policy = SplitPolicy::default();
        let mut t = Table::new(100 * MB);
        t.assign(1, start, &policy, true);
        t.assign(2, start, &policy, true);
        t.best_speed = 8.0 * MB as f64;
        // Segment 1 has received nothing; segment 0 downloads.
        t.segs[0].received += 4 * MB;
        t.segs[0].speed = 8.0 * MB as f64;
        // Still waiting for its response: some servers are that slow to answer.
        assert_eq!(t.laggard(start + Duration::from_secs(9)), None);
        // Answered, then nothing.
        t.answered(1, start);
        assert_eq!(t.laggard(start + Duration::from_secs(4)), None);
        assert_eq!(t.laggard(start + Duration::from_secs(5)), Some(1));
        // Also when it is the last one left.
        t.release(0, false, start);
        assert_eq!(t.laggard(start + Duration::from_secs(5)), Some(1));
        // When no connection has received anything yet, it is the network, not the connection.
        t.best_speed = 0.0;
        assert_eq!(t.laggard(start + Duration::from_secs(9)), None);
    }

    #[test]
    fn equally_slow_connections_are_not_dropped() {
        let start = Instant::now();
        let now = start + Duration::from_secs(3);
        let policy = SplitPolicy::default();
        let mut t = Table::new(100 * MB);
        for id in 1..=4 {
            t.assign(id, start, &policy, true);
        }
        t.best_speed = 8.0 * MB as f64;
        for s in &mut t.segs {
            s.speed = 64.0 * 1024.0; // a server capped per connection
        }
        assert_eq!(t.laggard(now), None);
    }

    #[test]
    fn a_busy_servers_wait_is_kept_when_longer_than_the_backoff() {
        let now = Instant::now();
        let policy = SplitPolicy::default();
        let mut t = Table::new(10 * MB);
        t.assign(1, now, &policy, true);
        t.release(0, true, now);
        t.hold(0, now + Duration::from_secs(5));
        assert_eq!(
            t.assign(2, now + Duration::from_secs(4), &policy, true),
            None
        );
        assert_eq!(
            t.assign(2, now + Duration::from_secs(5), &policy, true),
            Some(0)
        );
        // A shorter wait does not cut the backoff short.
        t.release(0, true, now);
        t.hold(0, now);
        assert!(t.segs[0].retry_at > Some(now));
    }

    #[test]
    fn unowned_failed_segment_waits_for_backoff() {
        let now = Instant::now();
        let policy = SplitPolicy::default();
        let mut t = Table::new(10 * MB);
        t.assign(1, now, &policy, true);
        t.release(0, true, now);
        // Not yet retryable: the only option is stealing, which needs an owned segment.
        assert_eq!(t.assign(2, now, &policy, true), None);
        assert_eq!(
            t.assign(2, now + Duration::from_secs(1), &policy, true),
            Some(0)
        );
    }

    #[test]
    fn does_not_split_below_min_piece() {
        let now = Instant::now();
        let policy = SplitPolicy::default();
        let mut t = Table::new(300 << 10);
        t.assign(1, now, &policy, true);
        assert_eq!(t.assign(2, now, &policy, true), None);
    }

    #[test]
    fn saved_roundtrip_keeps_durable_progress_only() {
        let mut t = Table::new(10 * MB);
        t.segs[0].received = 5 * MB;
        t.segs[0].written = 3 * MB;
        let restored = Table::from_saved(&t.saved());
        assert_eq!(restored.segs[0].received, 3 * MB);
        assert_eq!(restored.received_total(), 3 * MB);
    }

    /// A segment owned since `since`, `received` into it, at `speed` MiB/s.
    fn running(range: (u64, u64), received: u64, speed: f64, since: Instant) -> Segment {
        let mut s = Segment::new(range.0, range.1);
        s.received = received;
        s.written = received;
        s.owner = Some(range.0);
        s.owned_since = Some(since);
        s.speed = speed * MB as f64;
        s
    }

    fn table(segs: Vec<Segment>, best: f64) -> Table {
        Table {
            segs,
            best_speed: best * MB as f64,
        }
    }

    #[test]
    fn speeds_are_smoothed_per_connection() {
        let now = Instant::now();
        let policy = SplitPolicy::default();
        let mut t = Table::new(100 * MB);
        t.assign(1, now, &policy, true);
        t.assign(2, now, &policy, true);
        t.segs[0].received += 2 * MB;
        t.segs[1].received += MB;
        t.sample_speeds(Duration::from_millis(500));
        // The first sample is taken as it is: 2 MiB in half a second.
        assert_eq!(t.segs[0].speed, 4.0 * MB as f64);
        assert_eq!(t.segs[1].speed, 2.0 * MB as f64);
        // Then each new one counts a quarter: 0.75 × 4 + 0.25 × 8 MiB/s, and 0.75 × 2.
        t.segs[0].received += 4 * MB;
        t.sample_speeds(Duration::from_millis(500));
        assert_eq!(t.segs[0].speed, 5.0 * MB as f64);
        assert_eq!(t.segs[1].speed, 1.5 * MB as f64);
        assert_eq!(t.total_speed(), 6.5 * MB as f64);
        assert_eq!(t.best_speed(), 5.0 * MB as f64);
        // A released segment has no speed; the best one seen stays.
        t.release(0, false, now);
        t.sample_speeds(Duration::from_millis(500));
        assert_eq!(t.segs[0].speed, 0.0);
        assert_eq!(t.best_speed(), 5.0 * MB as f64);
    }

    #[test]
    fn typical_speed_is_the_median_of_measured_connections() {
        let now = Instant::now();
        let r =
            |i: u64, speed: f64| running((i * 10 * MB, (i + 1) * 10 * MB), i * 10 * MB, speed, now);
        let mut t = table(
            vec![r(0, 3.0), r(1, 1.0), r(2, 0.0), r(3, 4.0), r(4, 2.0)],
            0.0,
        );
        // Measured: 1, 2, 3, 4 MiB/s; of an even number, the upper middle one.
        assert_eq!(t.typical_speed(), Some(3.0 * MB as f64));
        assert_eq!(t.unmeasured(), 1);
        // A segment nobody owns does not count, whatever speed it last had.
        t.segs[3].owner = None;
        assert_eq!(t.typical_speed(), Some(2.0 * MB as f64));
        assert_eq!(t.unmeasured(), 1);
        let none = table(vec![r(0, 0.0)], 0.0);
        assert_eq!((none.typical_speed(), none.unmeasured()), (None, 1));
    }

    #[test]
    fn a_split_lets_victim_and_thief_finish_together() {
        let now = Instant::now();
        let policy = SplitPolicy::default();
        // The victim has 40 MiB left at 2 MiB/s; the thief is expected to go at the median of
        // the other measured connections (4 and 8: 8 MiB/s), so the victim keeps a fifth. One
        // not measured yet is assumed to go at the best speed seen and finishes sooner.
        let mut t = table(
            vec![
                running((0, 50 * MB), 10 * MB, 2.0, now),
                running((50 * MB, 60 * MB), 59 * MB, 4.0, now),
                running((60 * MB, 70 * MB), 69 * MB, 8.0, now),
                running((70 * MB, 80 * MB), 70 * MB, 0.0, now),
            ],
            8.0,
        );
        assert_eq!(t.assign(9, now, &policy, true), Some(4));
        assert_eq!(t.segs[0].end, 18 * MB);
        assert_eq!((t.segs[4].start, t.segs[4].end), (18 * MB, 50 * MB));

        // No other connection measured: the best speed seen stands for the thief.
        let mut t = table(vec![running((0, 50 * MB), 10 * MB, 2.0, now)], 6.0);
        t.assign(9, now, &policy, true);
        assert_eq!(t.segs[0].end, 20 * MB);

        // A victim not measured yet is halved.
        let mut t = table(vec![running((0, 100 * MB), 0, 0.0, now)], 4.0);
        t.assign(9, now, &policy, true);
        assert_eq!(t.segs[0].end, 50 * MB);
    }

    #[test]
    fn a_slow_victim_keeps_what_may_be_in_flight() {
        let now = Instant::now();
        let policy = SplitPolicy::default();
        // 0.3 s at its speed when that is more than the split gives it...
        let mut t = table(
            vec![
                running((0, 50 * MB), 10 * MB, 1.0, now),
                running((50 * MB, 60 * MB), 59 * MB, 199.0, now),
            ],
            199.0,
        );
        t.assign(9, now, &policy, true);
        assert_eq!(t.segs[0].end, 10 * MB + 314_572);
        // ...and never less than 64 KiB.
        let mut t = table(
            vec![
                running((0, 20 * MB), 10 * MB, 0.1, now),
                running((50 * MB, 60 * MB), 59 * MB, 50.0, now),
            ],
            50.0,
        );
        t.assign(9, now, &policy, true);
        assert_eq!(t.segs[0].end, 10 * MB + (64 << 10));
    }

    #[test]
    fn splits_stop_at_their_limits() {
        let now = Instant::now();
        let policy = SplitPolicy::default();
        let piece = policy.min_piece;
        // Exactly two pieces left: split in two.
        let mut t = table(vec![running((0, 2 * piece), 0, 0.0, now)], 0.0);
        assert_eq!(t.assign(9, now, &policy, true), Some(1));
        assert_eq!(t.segs[0].end, piece);
        // Less than two pieces left: not split, even when a crawling victim would keep little.
        let mut t = table(
            vec![
                running((0, 2 * piece - 1), 0, 0.001, now),
                running((MB, MB + piece), MB, 8.0, now),
            ],
            8.0,
        );
        assert_eq!(t.assign(9, now, &policy, true), None);
        // Expected to finish in exactly the shortest time worth splitting (0.4 s): split, the
        // victim keeping 0.3 s of it...
        let mut t = table(vec![running((0, 4 * MB), 0, 10.0, now)], 10.0);
        assert_eq!(t.assign(9, now, &policy, true), Some(1));
        assert_eq!(t.segs[0].end, 3 * MB);
        // ...sooner (0.35 s): left alone.
        let mut t = table(vec![running((0, 3 * MB + MB / 2), 0, 10.0, now)], 10.0);
        assert_eq!(t.assign(9, now, &policy, true), None);
    }

    #[test]
    fn failures_without_progress_back_off_up_to_eight_seconds() {
        let start = Instant::now();
        let policy = SplitPolicy::default();
        let mut t = Table::new(10 * MB);
        let mut now = start;
        for wait in [500, 1000, 2000, 4000, 8000, 8000] {
            assert_eq!(t.assign(1, now, &policy, false), Some(0));
            t.release(0, true, now);
            assert_eq!(t.segs[0].retry_at, Some(now + Duration::from_millis(wait)));
            now += Duration::from_millis(wait);
        }
        // A failure after progress is tried again at once, the count started over.
        t.assign(1, now, &policy, false);
        t.segs[0].received += MB;
        t.segs[0].written += MB;
        t.release(0, true, now);
        assert_eq!((t.segs[0].retry_at, t.segs[0].failures), (None, 0));
    }

    #[test]
    fn crawlers_are_dropped_only_when_a_fresh_connection_would_do_better() {
        let start = Instant::now();
        let now = start + Duration::from_secs(3);
        let r = |i: u64, speed: f64, since: Instant| {
            running(
                (i * 50 * MB, (i + 1) * 50 * MB),
                i * 50 * MB + 10 * MB,
                speed,
                since,
            )
        };
        // Typical: the median of connections measured for a second or more (8 and 10 MiB/s
        // with the crawler: 8); a young fast one and one not measured do not count.
        let young = now - Duration::from_millis(500);
        let crowd = |crawler: f64| {
            table(
                vec![
                    r(0, crawler, start),
                    r(1, 8.0, start),
                    r(2, 10.0, start),
                    r(3, 100.0, young),
                    r(4, 100.0, young),
                    r(5, 0.0, start),
                ],
                100.0,
            )
        };
        assert_eq!(crowd(1.0).laggard(now), Some(0));
        assert_eq!(crowd(1.3).laggard(now), None);
        assert_eq!(crowd(2.0).laggard(now), None);
        let at_the_limit = crowd(0.15 * 8.0);
        assert_eq!(at_the_limit.laggard(now), None);
        // Of an even number measured, the upper middle one: 1, 8, 10, 12 → 10.
        let mut even = crowd(1.4);
        even.segs[3].owned_since = Some(start);
        even.segs[3].speed = 12.0 * MB as f64;
        assert_eq!(even.laggard(now), Some(0));
        // Two not measured for long do not drag the median down to the crawler.
        let mut slow = crowd(1.0);
        slow.segs[3].owned_since = Some(start);
        slow.segs[3].speed = 0.0;
        assert_eq!(slow.laggard(now), Some(0));
        // Owned for exactly two seconds is long enough; dropped three times is enough.
        let mut t = crowd(1.0);
        t.segs[0].owned_since = Some(now - Duration::from_secs(2));
        assert_eq!(t.laggard(now), Some(0));
        t.segs[0].evictions = 3;
        assert_eq!(t.laggard(now), None);
        // Nearly done: reconnecting would cost more than the crawl.
        let mut t = crowd(0.1);
        t.segs[0].received = t.segs[0].end - (16 << 10);
        assert_eq!(t.laggard(now), None);
        t.segs[0].received = t.segs[0].end - (32 << 10);
        assert_eq!(t.laggard(now), Some(0));
        // Of two crawlers, the one with more left.
        let mut t = crowd(1.0);
        for i in [3, 4] {
            t.segs[i].owned_since = Some(start);
        }
        t.segs[1].speed = MB as f64;
        t.segs[1].received -= MB;
        assert_eq!(t.laggard(now), Some(1));
    }

    #[test]
    fn a_request_is_held_back_only_while_another_is_answered() {
        let now = Instant::now();
        let policy = SplitPolicy::default();
        let mut t = Table::new(100 * MB);
        for id in 0..3 {
            t.assign(id, now, &policy, true);
        }
        // Nobody answered yet: a slow server, not one holding requests back.
        assert!(!t.held_back(1));
        t.answered(0, now);
        assert!(t.held_back(1) && t.held_back(2) && !t.held_back(0));
        // A new request of an answered connection waits again.
        t.asking(0);
        t.answered(1, now);
        assert!(t.held_back(0) && !t.held_back(1));
        // Only a running segment can be held back.
        t.release(2, false, now);
        assert!(!t.held_back(2));
    }

    #[test]
    fn rewinding_starts_a_segment_over() {
        let now = Instant::now();
        let mut t = Table::new(10 * MB);
        t.assign(1, now, &SplitPolicy::default(), false);
        t.segs[0].received = 3 * MB;
        t.segs[0].written = 2 * MB;
        t.rewind(0);
        assert_eq!((t.segs[0].received, t.segs[0].written), (0, 0));
        assert_eq!(t.received_total(), 0);
    }
}
