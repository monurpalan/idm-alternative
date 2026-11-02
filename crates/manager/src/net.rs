//! Network and power: what the system says about the connection and the battery, the user's
//! rules for them (videos and torrents wait on a metered network, a speed limit on battery, a
//! proxy on a named network) and a monthly data budget.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::task::{Category, Status, Task, TaskId};
use crate::{Event, Inner, Result, Stop, lock, now_ms};

/// What the system says now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Conditions {
    /// The connection costs by the byte (mobile data, a hotspot), or the user marked it so.
    pub metered: bool,
    pub on_battery: bool,
    /// The network's name, where the system tells it (Wi-Fi: often only with location access).
    pub network: Option<String>,
}

/// Reads the [`Conditions`] (the app's reads the system; tests set their own).
#[derive(Clone)]
pub struct Sensor(pub Arc<dyn Fn() -> Conditions + Send + Sync>);

impl std::fmt::Debug for Sensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sensor")
    }
}

/// The user's rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetRules {
    /// On a metered network, videos and torrents wait (running ones go back to the queue).
    pub metered_hold: bool,
    /// On battery, all downloads together at most this many bytes per second.
    pub battery_limit: Option<u64>,
    /// On the network of this name, downloads go through this proxy.
    pub proxies: Vec<NetworkProxy>,
    /// Bytes a month (by the local calendar).
    pub monthly_budget: Option<u64>,
    /// Past the budget, downloads wait until the next month (or a larger budget); otherwise
    /// it is only said.
    pub budget_stop: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkProxy {
    pub network: String,
    pub proxy: String,
}

/// Bytes downloaded in a month.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// `2026-11`, by the local calendar.
    pub month: String,
    pub bytes: u64,
}

impl Usage {
    /// Adds `bytes` downloaded at `at` (Unix ms); a new month starts from zero.
    pub fn add(&mut self, at: i64, bytes: u64) {
        let month = month_of(at);
        if month != self.month {
            *self = Usage { month, bytes: 0 };
        }
        self.bytes += bytes;
    }

    /// This month's bytes as of `at` (zero once the month has turned).
    pub fn now(&self, at: i64) -> u64 {
        if month_of(at) == self.month {
            self.bytes
        } else {
            0
        }
    }
}

/// `YYYY-MM` of Unix milliseconds, in local time.
pub fn month_of(at: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(at)
        .single()
        .map(|t| t.format("%Y-%m").to_string())
        .unwrap_or_default()
}

/// The network and power state for the settings and MCP.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NetStatus {
    pub conditions: Conditions,
    pub usage: Usage,
    pub budget: Option<u64>,
    /// Downloads wait because the budget is used up.
    pub over_budget: bool,
}

/// Said once a month each: the budget nearly (90%) or wholly used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BudgetNotice {
    pub used: u64,
    pub budget: u64,
    /// Downloads were stopped (the rule says to), not only warned about.
    pub stopped: bool,
}

/// What the network watch keeps between looks.
#[derive(Debug, Default)]
pub(crate) struct NetState {
    pub(crate) conditions: Conditions,
    /// The limit for all downloads the user set; on battery the tighter one applies.
    pub(crate) user_limit: Option<u64>,
    pub(crate) usage: Usage,
    /// Each download's last `received`, for counting what arrived since.
    seen: HashMap<TaskId, u64>,
    /// `month:90` / `month:100` once said.
    warned: Vec<String>,
}

const KEY: &str = "data-usage";

/// Videos and torrents: what waits on a metered network.
pub(crate) fn heavy(task: &Task) -> bool {
    task.category == Category::Video
        || task.stream.is_some()
        || crate::torrent::is_torrent(&task.url)
}

impl Inner {
    /// Why the queue does not start `task` now, if it does not.
    pub(crate) fn held(&self, task: &Task) -> Option<&'static str> {
        let rules = lock(&self.config).net_rules.clone();
        let net = lock(&self.net);
        let over = rules
            .monthly_budget
            .is_some_and(|b| rules.budget_stop && net.usage.now(now_ms()) >= b);
        if over {
            Some("waiting: the monthly data budget is used up")
        } else if rules.metered_hold && net.conditions.metered && heavy(task) {
            Some("waiting: videos and torrents wait on a metered network")
        } else {
            None
        }
    }

    /// The proxy the rules give for the network now, if any.
    pub(crate) fn network_proxy(&self) -> Option<String> {
        let rules = lock(&self.config).net_rules.clone();
        let network = lock(&self.net).conditions.network.clone()?;
        rules
            .proxies
            .iter()
            .find(|p| p.network.eq_ignore_ascii_case(network.trim()) && !p.proxy.trim().is_empty())
            .map(|p| p.proxy.trim().to_owned())
    }

    /// The limit for all downloads: the user's, or on battery the rule's when tighter.
    pub(crate) fn apply_limit(&self) {
        let battery = lock(&self.config).net_rules.battery_limit;
        let (user, on_battery) = {
            let net = lock(&self.net);
            (net.user_limit, net.conditions.on_battery)
        };
        let limit = match (user, battery.filter(|_| on_battery)) {
            (Some(u), Some(b)) => Some(u.min(b)),
            (u, b) => u.or(b),
        };
        self.global.set_rate(limit);
        self.torrents.set_limit(limit);
    }

    /// Looks at the network and the power, applies the rules, counts what was downloaded
    /// and keeps the budget.
    pub(crate) async fn sense(self: &Arc<Self>) -> Result<()> {
        let sensor = lock(&self.config).sensor.clone();
        if let Some(Sensor(read)) = sensor {
            let now = tokio::task::spawn_blocking(move || read())
                .await
                .unwrap_or_default();
            lock(&self.net).conditions = now;
        }
        self.apply_limit();
        let usage = lock(&self.net).usage.clone();
        self.db
            .save_setting(KEY, &serde_json::to_string(&usage)?)
            .await?;
        self.budget_notice(&usage);
        // Running downloads the rules hold now go back to the queue.
        let running: Vec<TaskId> = lock(&self.running).keys().copied().collect();
        for id in running {
            let Some(task) = self.db.get(id).await? else {
                continue;
            };
            let Some(why) = self.held(&task) else {
                continue;
            };
            tracing::info!(id, "{why}");
            self.stop(id, Stop::Pause).await;
            self.db.set_status(id, Status::Queued, Some(why)).await?;
            if let Some(t) = self.db.get(id).await? {
                self.emit(Event::Updated(t));
            }
        }
        self.fill().await
    }

    /// Says when the budget is 90% and when it is wholly used, once a month each.
    fn budget_notice(&self, usage: &Usage) {
        let (budget, stop) = {
            let cfg = lock(&self.config);
            (cfg.net_rules.monthly_budget, cfg.net_rules.budget_stop)
        };
        let Some(budget) = budget.filter(|b| *b > 0) else {
            return;
        };
        let level = if usage.bytes >= budget {
            100
        } else if usage.bytes * 10 >= budget * 9 {
            90
        } else {
            return;
        };
        let key = format!("{}:{level}", usage.month);
        let mut net = lock(&self.net);
        if net.warned.contains(&key) {
            return;
        }
        net.warned.push(key);
        drop(net);
        self.emit(Event::Budget(BudgetNotice {
            used: usage.bytes,
            budget,
            stopped: stop && level == 100,
        }));
    }

    /// Counts the bytes in a progress report since the download's last one.
    pub(crate) fn count(&self, id: TaskId, received: u64) {
        let mut net = lock(&self.net);
        let last = net.seen.insert(id, received).unwrap_or(received);
        // Less than before: it started over; the next reports count from here.
        let new = received.saturating_sub(last);
        if new > 0 {
            net.usage.add(now_ms(), new);
        }
    }

    /// Looks at the network and the power now and then, and counts the bytes downloads
    /// report; both end with the manager.
    pub(crate) fn watch_network(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let every = lock(&self.config).sense_every;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(inner) = weak.upgrade() else { return };
                if inner.closed.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                if let Err(e) = inner.sense().await {
                    tracing::warn!("could not apply the network rules: {e}");
                }
            }
        });
        let weak = Arc::downgrade(self);
        let mut events = self.events.subscribe();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(Event::Progress(p)) => match weak.upgrade() {
                        Some(inner) => inner.count(p.id, p.received),
                        None => return,
                    },
                    Ok(Event::Removed(id)) => {
                        if let Some(inner) = weak.upgrade() {
                            lock(&inner.net).seen.remove(&id);
                        }
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    /// This month's usage, as saved when the list was last open.
    pub(crate) async fn load_usage(&self) -> Result<()> {
        let saved: Option<Usage> = self
            .db
            .setting(KEY)
            .await?
            .and_then(|s| serde_json::from_str(&s).ok());
        if let Some(u) = saved.filter(|u| u.month == month_of(now_ms())) {
            lock(&self.net).usage = u;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_starts_again_each_month() {
        use chrono::TimeZone;
        let at = |y, m, d| {
            chrono::Local
                .with_ymd_and_hms(y, m, d, 12, 0, 0)
                .single()
                .expect("time")
                .timestamp_millis()
        };
        let mut u = Usage::default();
        u.add(at(2026, 10, 30), 100);
        u.add(at(2026, 10, 31), 50);
        assert_eq!((u.month.as_str(), u.bytes), ("2026-10", 150));
        assert_eq!(
            u.now(at(2026, 11, 1)),
            0,
            "a new month reads zero before anything"
        );
        u.add(at(2026, 11, 1), 7);
        assert_eq!((u.month.as_str(), u.bytes), ("2026-11", 7));
        assert_eq!(month_of(at(2027, 1, 15)), "2027-01");
    }

    #[test]
    fn what_waits_on_a_metered_network() {
        let video = crate::task::tests::sample("https://x/a.mp4", "a.mp4");
        let zip = crate::task::tests::sample("https://x/a.zip", "a.zip");
        let torrent = crate::task::tests::sample("magnet:?xt=urn:btih:abc", "t");
        let mut page = zip.clone();
        page.stream = Some(crate::task::Stream::default());
        assert!(heavy(&video) && heavy(&torrent) && heavy(&page));
        assert!(!heavy(&zip));
    }
}
