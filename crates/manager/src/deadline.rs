//! Downloads with a deadline: the user's ("by 18:00"), or, without one, when the link is
//! expected to stop working by how long the site's links lasted. The queue starts the
//! earliest deadline first; while one waits for a slot or falls behind, downloads without a
//! deadline go back to the queue; and one that will not make it is said early.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;

use crate::sites;
use crate::task::{Status, Task, TaskId};
use crate::{Event, Inner, Manager, Result, Stop, lock, now_ms, site_of};

/// A deadline is at risk when what is left would take longer at the speed now than the time
/// left, with this much to spare.
const MARGIN: f64 = 1.1;

/// Where a deadline comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// The user set it.
    Own,
    /// When the link is expected to stop working.
    Link,
}

/// How a download with a deadline is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    OnTime,
    /// At the speed now, it would finish after the deadline.
    AtRisk,
    /// The deadline has passed.
    Late,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeadlineView {
    pub id: TaskId,
    /// Unix milliseconds.
    pub at: i64,
    pub source: Source,
    pub standing: Standing,
    /// Bytes per second it needs from now on to make it, when its size is known.
    pub needed: Option<f64>,
}

/// Said once per download: it will not make its deadline at the speed now.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeadlineRisk {
    pub id: TaskId,
    pub name: Option<String>,
    pub at: i64,
    pub needed: f64,
    pub speed: f64,
}

/// What the deadline watch keeps between looks.
#[derive(Debug, Default)]
pub(crate) struct DeadlineState {
    /// Each running download's speed in its last progress report.
    pub(crate) speed: HashMap<TaskId, (u64, f64)>,
    /// Downloads with a deadline wait for a slot or fall behind: the others wait.
    pub(crate) pressure: bool,
    /// Each unfinished download's deadline, its own or its link's, as last looked at.
    pub(crate) at: HashMap<TaskId, i64>,
    /// Downloads already said to be at risk.
    warned: Vec<TaskId>,
}

/// Where `task` stands against `at` at `speed` bytes per second, `now` being now.
pub fn standing(
    task: &Task,
    received: u64,
    speed: f64,
    at: i64,
    now: i64,
) -> (Standing, Option<f64>) {
    let left_secs = (at - now) as f64 / 1000.0;
    if left_secs <= 0.0 {
        return (Standing::Late, None);
    }
    let Some(size) = task.size else {
        return (Standing::OnTime, None);
    };
    let remaining = size.saturating_sub(received) as f64;
    let needed = remaining / left_secs;
    // Before it has a speed (still waiting), it is on time unless nothing could make it.
    let at_risk = task.status == Status::Downloading && speed * MARGIN < needed;
    (
        if at_risk {
            Standing::AtRisk
        } else {
            Standing::OnTime
        },
        Some(needed),
    )
}

impl Inner {
    /// The download's deadline: its own, or when its link is expected to expire.
    pub(crate) async fn deadline_of(&self, task: &Task) -> Result<Option<(i64, Source)>> {
        if let Some(at) = task.deadline {
            return Ok(Some((at, Source::Own)));
        }
        if task.status == Status::Done {
            return Ok(None);
        }
        let Some(site) = site_of(&task.url) else {
            return Ok(None);
        };
        let lifetime = sites::recall(&self.db, &site, now_ms())
            .await?
            .and_then(|m| m.link_lifetime);
        Ok(lifetime.map(|l| (task.link_at.saturating_add(l as i64 * 1000), Source::Link)))
    }

    /// Looks at the downloads with a deadline: says the ones falling behind, and makes the
    /// others wait while one of them waits for a slot or falls behind.
    pub(crate) async fn watch_deadlines(self: &Arc<Self>) -> Result<()> {
        let now = now_ms();
        let tasks = self.db.list().await?;
        let full = lock(&self.running).len() >= lock(&self.config).max_active.max(1);
        let mut pressure = false;
        let mut all = HashMap::new();
        for task in tasks
            .iter()
            .filter(|t| matches!(t.status, Status::Queued | Status::Downloading))
        {
            let Some((at, _)) = self.deadline_of(task).await? else {
                continue;
            };
            all.insert(task.id, at);
            let (received, speed) = lock(&self.deadlines)
                .speed
                .get(&task.id)
                .copied()
                .unwrap_or((task.received, 0.0));
            let (standing, needed) = standing(task, received, speed, at, now);
            // Waiting for a slot (not held for another reason), or falling behind.
            let waiting = full && task.status == Status::Queued && self.held_by_net(task).is_none();
            pressure |= standing != Standing::Late && (waiting || standing == Standing::AtRisk);
            if standing == Standing::AtRisk {
                self.warn_risk(task, at, needed.unwrap_or_default(), speed);
            }
        }
        {
            let mut state = lock(&self.deadlines);
            state.pressure = pressure;
            state.at = all;
        }
        if pressure {
            self.make_room().await?;
        }
        self.fill().await
    }

    /// Sends one running download without a deadline back to the queue, freeing its slot and
    /// its share of the line.
    async fn make_room(self: &Arc<Self>) -> Result<()> {
        let running: Vec<TaskId> = lock(&self.running).keys().copied().collect();
        for id in running {
            let Some(task) = self.db.get(id).await? else {
                continue;
            };
            if self.deadline_of(&task).await?.is_some() {
                continue;
            }
            let why = "waiting: downloads with a deadline go first";
            tracing::info!(id, "{why}");
            self.stop(id, Stop::Pause).await;
            self.db.set_status(id, Status::Queued, Some(why)).await?;
            if let Some(t) = self.db.get(id).await? {
                self.emit(Event::Updated(t));
            }
            return Ok(());
        }
        Ok(())
    }

    fn warn_risk(&self, task: &Task, at: i64, needed: f64, speed: f64) {
        let mut state = lock(&self.deadlines);
        if state.warned.contains(&task.id) {
            return;
        }
        state.warned.push(task.id);
        drop(state);
        self.emit(Event::DeadlineRisk(DeadlineRisk {
            id: task.id,
            name: task.name.clone(),
            at,
            needed,
            speed,
        }));
    }

    /// Keeps each running download's speed, from its progress reports.
    pub(crate) fn note_speed(&self, id: TaskId, received: u64, speed: f64) {
        lock(&self.deadlines).speed.insert(id, (received, speed));
    }

    /// Looks at the deadlines every second; ends with the manager.
    pub(crate) fn watch_deadlines_often(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(inner) = weak.upgrade() else { return };
                if inner.closed.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                if let Err(e) = inner.watch_deadlines().await {
                    tracing::warn!("could not look at the deadlines: {e}");
                }
            }
        });
    }
}

impl Manager {
    /// Sets (or with `None` clears) a download's deadline, Unix milliseconds.
    pub async fn set_deadline(&self, id: TaskId, at: Option<i64>) -> Result<Task> {
        self.inner.db.set_deadline(id, at).await?;
        lock(&self.inner.deadlines).warned.retain(|w| *w != id);
        let task = self.get(id).await?;
        self.inner.emit(Event::Updated(task.clone()));
        self.inner.fill().await?;
        Ok(task)
    }

    /// Every unfinished download with a deadline (its own, or its link's), and how it stands.
    pub async fn deadlines(&self) -> Result<Vec<DeadlineView>> {
        let now = now_ms();
        let mut out = Vec::new();
        for task in self.inner.db.list().await? {
            if task.status == Status::Done {
                continue;
            }
            let Some((at, source)) = self.inner.deadline_of(&task).await? else {
                continue;
            };
            let (received, speed) = lock(&self.inner.deadlines)
                .speed
                .get(&task.id)
                .copied()
                .unwrap_or((task.received, 0.0));
            let (standing, needed) = standing(&task, received, speed, at, now);
            out.push(DeadlineView {
                id: task.id,
                at,
                source,
                standing,
                needed,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standing_against_the_clock() {
        let mut t = crate::task::tests::sample("https://x/a.bin", "a.bin");
        t.status = Status::Downloading;
        t.size = Some(10_000_000);
        let now = 1_000_000;
        // 10 MB in 10 s needs 1 MB/s.
        let (s, needed) = standing(&t, 0, 2_000_000.0, now + 10_000, now);
        assert_eq!((s, needed), (Standing::OnTime, Some(1_000_000.0)));
        assert_eq!(
            standing(&t, 0, 500_000.0, now + 10_000, now).0,
            Standing::AtRisk
        );
        // Half done: half the speed is enough.
        assert_eq!(
            standing(&t, 5_000_000, 600_000.0, now + 10_000, now).0,
            Standing::OnTime
        );
        assert_eq!(standing(&t, 0, 9e9, now - 1, now).0, Standing::Late);
        // Waiting in the queue, it has no speed yet: not at risk for that.
        t.status = Status::Queued;
        assert_eq!(standing(&t, 0, 0.0, now + 10_000, now).0, Standing::OnTime);
        // No size: nothing to measure against.
        t.size = None;
        t.status = Status::Downloading;
        assert_eq!(
            standing(&t, 0, 1.0, now + 10_000, now),
            (Standing::OnTime, None)
        );
    }
}
