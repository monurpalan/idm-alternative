//! A download's speed from its first byte to its end, for the graph in its details. One
//! point per `step` seconds of downloading, at most [`POINTS`]: when they are all used,
//! neighbours are merged and the step doubles, so the points always cover the whole
//! download in the same small space. Time paused or waiting is not in it.
//!
//! A running download's history is kept in memory and written to the list when it stops
//! (paused, failed, finished, or the manager closing); a resumed download goes on from it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use shardload_engine::Phase;

use crate::db::Db;
use crate::{TaskId, lock};

/// Points kept at most; even, so a full history merges into exactly half.
pub const POINTS: usize = 300;

/// How often a running download's speed goes into its history.
const SAMPLE_EVERY: Duration = Duration::from_secs(1);

/// What a download's speed was, oldest first.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpeedHistory {
    /// Seconds of downloading each point averages.
    pub step: u32,
    /// Bytes per second.
    pub points: Vec<u64>,
    /// Seconds already in the point being filled, and their speeds added up.
    pub count: u32,
    pub sum: f64,
}

impl Default for SpeedHistory {
    fn default() -> Self {
        Self {
            step: 1,
            points: Vec::new(),
            count: 0,
            sum: 0.0,
        }
    }
}

impl SpeedHistory {
    /// One more second of downloading at `speed` bytes per second.
    pub fn push(&mut self, speed: f64) {
        self.sum += speed.max(0.0);
        self.count += 1;
        if self.count < self.step.max(1) {
            return;
        }
        self.points
            .push((self.sum / f64::from(self.count)).round() as u64);
        self.sum = 0.0;
        self.count = 0;
        if self.points.len() >= POINTS {
            self.points = self
                .points
                .chunks(2)
                .map(|pair| pair.iter().sum::<u64>() / pair.len() as u64)
                .collect();
            self.step = self.step.max(1) * 2;
        }
    }

    /// Seconds of downloading it covers.
    pub fn seconds(&self) -> u64 {
        self.points.len() as u64 * u64::from(self.step) + u64::from(self.count)
    }
}

/// The histories of the downloads running now, by id.
pub(crate) type Live = Arc<Mutex<HashMap<TaskId, SpeedHistory>>>;

/// Keeps a running download's history from its progress: a second at a time while it
/// downloads, in [`Live`] until it stops.
pub(crate) struct Recorder {
    id: TaskId,
    live: Live,
    /// The speed as last reported, and whether it was downloading then.
    speed: f64,
    downloading: bool,
    /// When the speed last went into the history; `None` until it began downloading.
    sampled: Option<Instant>,
}

impl Recorder {
    /// Goes on from the history saved when the download last stopped.
    pub(crate) async fn open(id: TaskId, db: &Db, live: Live) -> Self {
        let history = match db.speeds(id).await {
            Ok(h) => h.unwrap_or_default(),
            Err(e) => {
                tracing::warn!(id, "could not read its speed history: {e}");
                SpeedHistory::default()
            }
        };
        lock(&live).insert(id, history);
        Self {
            id,
            live,
            speed: 0.0,
            downloading: false,
            sampled: None,
        }
    }

    /// New progress. Whether it is the first that downloads.
    pub(crate) fn saw(&mut self, phase: Phase, speed: f64) -> bool {
        self.speed = speed;
        // Not the time spent asking the server, checking the file or joining its parts.
        self.downloading = matches!(phase, Phase::Downloading | Phase::Recording);
        let first = self.downloading && self.sampled.is_none();
        if first {
            self.sampled = Some(Instant::now());
        }
        first
    }

    /// Called often: a second that has passed goes into the history.
    pub(crate) fn tick(&mut self) {
        let Some(at) = &mut self.sampled else {
            return;
        };
        if at.elapsed() < SAMPLE_EVERY {
            return;
        }
        *at += SAMPLE_EVERY;
        if self.downloading
            && let Some(h) = lock(&self.live).get_mut(&self.id)
        {
            h.push(self.speed);
        }
    }

    /// The download stopped: its history is saved for the next start and for the graph.
    pub(crate) async fn close(self, db: &Db) {
        let history = lock(&self.live).remove(&self.id);
        if let Some(h) = history.filter(|h| h.seconds() > 0)
            && let Err(e) = db.set_speeds(self.id, &h).await
        {
            tracing::warn!(id = self.id, "could not save its speed history: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_point_a_second_at_first() {
        let mut h = SpeedHistory::default();
        for s in [100.0, 200.0, 300.0] {
            h.push(s);
        }
        assert_eq!(h.step, 1);
        assert_eq!(h.points, vec![100, 200, 300]);
        assert_eq!(h.seconds(), 3);
    }

    #[test]
    fn a_full_history_merges_neighbours_and_doubles_the_step() {
        let mut h = SpeedHistory::default();
        for i in 0..POINTS {
            h.push(i as f64 * 2.0);
        }
        assert_eq!(h.step, 2);
        assert_eq!(h.points.len(), POINTS / 2);
        // 0 and 2, 4 and 6...
        assert_eq!(h.points[..3], [1, 5, 9]);
        assert_eq!(h.seconds(), POINTS as u64);
    }

    #[test]
    fn stays_small_and_covers_the_whole_download() {
        let mut h = SpeedHistory::default();
        // Ten hours at 1000 B/s, then one at 5000 B/s.
        let hours = 11 * 3600;
        for i in 0..hours {
            h.push(if i < 10 * 3600 { 1000.0 } else { 5000.0 });
        }
        assert!(h.points.len() < POINTS, "{}", h.points.len());
        assert_eq!(h.seconds(), hours as u64);
        assert!(h.points.iter().all(|&p| (1000..=5000).contains(&p)));
        assert_eq!(*h.points.first().expect("points"), 1000);
        // The last hour is at the end, not lost in the merging.
        let tail = h.points.len() - h.points.len() / 11 + 1;
        assert!(
            h.points[tail..].iter().all(|&p| p == 5000),
            "{:?}",
            &h.points[tail..]
        );
    }

    #[test]
    fn a_point_half_filled_is_kept_across_a_save() {
        let mut h = SpeedHistory::default();
        for _ in 0..POINTS {
            h.push(10.0);
        }
        h.push(30.0);
        assert_eq!((h.step, h.count), (2, 1));
        let mut back: SpeedHistory =
            serde_json::from_str(&serde_json::to_string(&h).expect("json")).expect("parse");
        back.push(50.0);
        assert_eq!(back.points.last(), Some(&40));
        assert_eq!(back.count, 0);
    }
}
