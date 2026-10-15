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
            min_eta: Duration::from_secs(1),
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

    pub fn total_speed(&self) -> f64 {
        self.segs.iter().map(|s| s.speed).sum()
    }

    /// Gives segment `idx` to worker `id`.
    pub fn claim(&mut self, idx: usize, id: WorkerId, now: Instant) -> usize {
        let s = &mut self.segs[idx];
        s.owner = Some(id);
        s.owned_since = Some(now);
        s.retry_at = None;
        s.speed = 0.0;
        s.sampled = s.received;
        s.written_at_claim = s.written;
        idx
    }

    /// Gives worker `id` something to do: an unowned unfinished segment whose retry delay
    /// has passed, or a piece stolen from the slowest-to-finish segment.
    pub fn assign(&mut self, id: WorkerId, now: Instant, policy: &SplitPolicy) -> Option<usize> {
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

    /// A connection that has crawled for a while at a small fraction of the others' speed
    /// while more than a second of healthy work is left in its segment. The controller
    /// drops it so a fresh connection can take the range.
    pub fn laggard(&self, now: Instant) -> Option<usize> {
        let age = |s: &Segment| s.owned_since.map_or(Duration::ZERO, |t| now - t);
        let mut speeds: Vec<f64> = self
            .segs
            .iter()
            .filter(|s| s.owner.is_some() && s.speed > 0.0 && age(s) >= Duration::from_secs(1))
            .map(|s| s.speed)
            .collect();
        if speeds.len() < 2 {
            return None;
        }
        speeds.sort_by(f64::total_cmp);
        let typical = speeds[speeds.len() / 2];
        self.segs
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.owner.is_some()
                    && s.evictions < 3
                    && age(s) >= Duration::from_secs(2)
                    && s.speed < 0.15 * typical
                    && s.remaining() as f64 > typical
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
        assert_eq!(t.assign(1, now, &policy), Some(0));
        assert_eq!(t.assign(2, now, &policy), Some(1));
        assert_eq!((t.segs[0].start, t.segs[0].end), (0, 50 * MB));
        assert_eq!((t.segs[1].start, t.segs[1].end), (50 * MB, 100 * MB));
    }

    #[test]
    fn slow_victim_keeps_only_a_sliver() {
        let now = Instant::now();
        let policy = SplitPolicy::default();
        let mut t = Table::new(100 * MB);
        t.assign(1, now, &policy);
        t.assign(2, now, &policy);
        // Segment 0 crawls at 64 KiB/s; segment 1 is healthy at 8 MiB/s.
        t.segs[0].speed = 64.0 * 1024.0;
        t.segs[0].received = 10 * MB;
        t.segs[1].speed = 8.0 * MB as f64;
        t.segs[1].received = 60 * MB;
        assert_eq!(t.assign(3, now, &policy), Some(2));
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
    fn unowned_failed_segment_waits_for_backoff() {
        let now = Instant::now();
        let policy = SplitPolicy::default();
        let mut t = Table::new(10 * MB);
        t.assign(1, now, &policy);
        t.release(0, true, now);
        // Not yet retryable: the only option is stealing, which needs an owned segment.
        assert_eq!(t.assign(2, now, &policy), None);
        assert_eq!(t.assign(2, now + Duration::from_secs(1), &policy), Some(0));
    }

    #[test]
    fn does_not_split_below_min_piece() {
        let now = Instant::now();
        let policy = SplitPolicy::default();
        let mut t = Table::new(300 << 10);
        t.assign(1, now, &policy);
        assert_eq!(t.assign(2, now, &policy), None);
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
}
