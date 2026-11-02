//! "Why is it slow?": one-line hints in a running download's details, only for what is
//! known for certain and what the user can act on or should know. No speed test and no
//! guess at what the line could do: a download with none of these shows nothing.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::Serialize;
use shardload_engine::{Health, Limiter};

/// One thing holding a download back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Hint {
    /// A speed limit sets the pace: this download's own (`own`), or the one for all.
    Limit { rate: u64, own: bool },
    /// The disk cannot write as fast as the data arrives.
    Disk,
    /// Connections keep dropping: this many were opened again in the last minute.
    Drops { count: u64 },
    /// The server allows only this many connections.
    Server { connections: usize },
    /// Downloads go through the proxy set in the settings.
    Proxy,
}

/// The disk hint shows once the connections spend this share of their time writing, and
/// goes once it falls below [`DISK_OFF`]: a disk near the line does not make it flicker.
const DISK_ON: f64 = 0.5;
const DISK_OFF: f64 = 0.3;
/// Connections opened again within [`DROPS_WINDOW`] for the drops hint.
const DROPS_SHOWN: u64 = 3;
const DROPS_WINDOW: Duration = Duration::from_secs(60);
/// A limit counts as setting the pace once it has been saturated this long.
const LIMIT_SETTLED: Duration = Duration::from_secs(2);

/// What a download's hints are worked out from, besides its progress.
#[derive(Clone, Default)]
pub(crate) struct Watch {
    /// The limit for all downloads, and this download's own.
    pub(crate) global: Option<Limiter>,
    pub(crate) own: Option<Limiter>,
    pub(crate) proxy: bool,
}

/// Works out a running download's hints, tick by tick.
pub(crate) struct Hints {
    watch: Watch,
    disk: bool,
    drops: VecDeque<(Instant, u64)>,
    saturated_since: Option<Instant>,
}

impl Hints {
    pub(crate) fn new(watch: Watch) -> Self {
        Self {
            watch,
            disk: false,
            drops: VecDeque::new(),
            saturated_since: None,
        }
    }

    /// The hints for now, most useful first. `downloading`: data is flowing (hints mean
    /// nothing while asking the server or verifying).
    pub(crate) fn update(&mut self, health: &Health, downloading: bool, now: Instant) -> Vec<Hint> {
        if !downloading {
            self.saturated_since = None;
            return Vec::new();
        }
        let mut hints = Vec::new();
        if let Some(limit) = self.limit(now) {
            hints.push(limit);
        }
        self.disk = if self.disk {
            health.disk_wait >= DISK_OFF
        } else {
            health.disk_wait >= DISK_ON
        };
        if self.disk {
            hints.push(Hint::Disk);
        }
        let count = self.drops_lately(health.reconnects, now);
        if count >= DROPS_SHOWN {
            hints.push(Hint::Drops { count });
        }
        // Only what the server refused in this download: the site's memory tries one
        // connection more than it allowed before, and is shown with the site.
        if let Some(connections) = health.server_limit {
            hints.push(Hint::Server { connections });
        }
        if self.watch.proxy {
            hints.push(Hint::Proxy);
        }
        hints
    }

    /// The limit that has held the download at its rate for a while: the tighter of the two
    /// when both are saturated.
    fn limit(&mut self, now: Instant) -> Option<Hint> {
        let saturated = |l: &Option<Limiter>| {
            l.as_ref()
                .filter(|l| l.is_saturated())
                .and_then(Limiter::rate)
        };
        let own = saturated(&self.watch.own);
        let global = saturated(&self.watch.global);
        let hint = match (own, global) {
            (Some(o), Some(g)) if g < o => Hint::Limit {
                rate: g,
                own: false,
            },
            (Some(o), _) => Hint::Limit { rate: o, own: true },
            (None, Some(g)) => Hint::Limit {
                rate: g,
                own: false,
            },
            (None, None) => {
                self.saturated_since = None;
                return None;
            }
        };
        let since = *self.saturated_since.get_or_insert(now);
        (now - since >= LIMIT_SETTLED).then_some(hint)
    }

    /// Connections opened again within the last [`DROPS_WINDOW`], from the running total
    /// (which starts at 0 with the download).
    fn drops_lately(&mut self, total: u64, now: Instant) -> u64 {
        if self.drops.back().is_none_or(|(_, t)| *t != total) {
            self.drops.push_back((now, total));
        }
        let old = |at: &Instant| now - *at > DROPS_WINDOW;
        // The front stays the last count from before the window.
        while self.drops.get(1).is_some_and(|(at, _)| old(at)) {
            self.drops.pop_front();
        }
        let before = self
            .drops
            .front()
            .filter(|(at, _)| old(at))
            .map_or(0, |(_, t)| *t);
        total.saturating_sub(before)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(reconnects: u64, disk_wait: f64) -> Health {
        Health {
            reconnects,
            disk_wait,
            server_limit: None,
        }
    }

    #[test]
    fn a_plain_download_has_no_hints() {
        let mut h = Hints::new(Watch::default());
        let t0 = Instant::now();
        for i in 0..100 {
            let at = t0 + Duration::from_millis(250 * i);
            assert!(h.update(&health(1, 0.05), true, at).is_empty());
        }
    }

    #[test]
    fn disk_hint_has_hysteresis() {
        let mut h = Hints::new(Watch::default());
        let now = Instant::now();
        assert!(h.update(&health(0, 0.45), true, now).is_empty());
        assert_eq!(h.update(&health(0, 0.6), true, now), vec![Hint::Disk]);
        assert_eq!(h.update(&health(0, 0.35), true, now), vec![Hint::Disk]);
        assert!(h.update(&health(0, 0.2), true, now).is_empty());
    }

    #[test]
    fn drops_are_counted_over_the_last_minute() {
        let mut h = Hints::new(Watch::default());
        let t0 = Instant::now();
        assert!(h.update(&health(2, 0.0), true, t0).is_empty());
        let hints = h.update(&health(4, 0.0), true, t0 + Duration::from_secs(10));
        assert_eq!(hints, vec![Hint::Drops { count: 4 }]);
        // No more drops: after a minute the hint goes.
        let later = t0 + Duration::from_secs(80);
        assert!(h.update(&health(4, 0.0), true, later).is_empty());
        // Two more: under the threshold.
        let hints = h.update(&health(6, 0.0), true, later + Duration::from_secs(1));
        assert!(hints.is_empty(), "{hints:?}");
    }

    #[test]
    fn a_saturated_limit_shows_after_it_settles() {
        let own = Limiter::new(Some(1000));
        let mut h = Hints::new(Watch {
            own: Some(own.clone()),
            global: Some(Limiter::new(None)),
            ..Watch::default()
        });
        let t0 = Instant::now();
        assert!(h.update(&health(0, 0.0), true, t0).is_empty(), "never used");
        // Reservations waiting: saturated.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let _ = tokio::time::timeout(Duration::from_millis(5), own.acquire(64 << 10)).await;
        });
        assert!(
            h.update(&health(0, 0.0), true, t0).is_empty(),
            "not settled"
        );
        let hints = h.update(&health(0, 0.0), true, t0 + LIMIT_SETTLED);
        assert_eq!(
            hints,
            vec![Hint::Limit {
                rate: 1000,
                own: true
            }]
        );
        assert!(
            h.update(&health(0, 0.0), false, t0 + LIMIT_SETTLED)
                .is_empty()
        );
    }

    #[test]
    fn server_and_proxy_are_told() {
        let mut h = Hints::new(Watch {
            proxy: true,
            ..Watch::default()
        });
        let now = Instant::now();
        assert_eq!(h.update(&health(0, 0.0), true, now), vec![Hint::Proxy]);
        let mut learned = health(0, 0.0);
        learned.server_limit = Some(1);
        assert_eq!(
            h.update(&learned, true, now),
            vec![Hint::Server { connections: 1 }, Hint::Proxy]
        );
    }
}
