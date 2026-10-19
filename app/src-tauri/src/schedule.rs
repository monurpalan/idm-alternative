//! The scheduler (start and stop the queue at set times, like IDM's) and "when all downloads
//! finish": close the app, sleep, hibernate or shut down, after a countdown the user can stop.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{Datelike, NaiveDateTime, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};
use shardload_manager::{Event, Status};
use tauri::{AppHandle, Emitter, Manager as _};
use tokio::sync::broadcast::error::RecvError;

use crate::{App, lock};

/// When the queue starts and stops by itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub enabled: bool,
    /// `HH:MM`, local time; empty for none.
    pub start: String,
    pub stop: String,
    /// Monday first.
    pub days: [bool; 7],
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            enabled: false,
            start: "02:00".into(),
            stop: "08:00".into(),
            days: [true; 7],
        }
    }
}

impl Schedule {
    pub fn validate(&self) -> Result<(), String> {
        for t in [&self.start, &self.stop] {
            if !t.is_empty() && parse_time(t).is_none() {
                return Err(format!("{t} is not a time (HH:MM)"));
            }
        }
        if self.enabled && self.start.is_empty() && self.stop.is_empty() {
            return Err("the scheduler needs a start or a stop time".into());
        }
        Ok(())
    }
}

pub fn parse_time(text: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(text.trim(), "%H:%M").ok()
}

/// A wake-up later than this after the previous one (the computer slept) does not fire what
/// it skipped: starting downloads hours late could surprise more than it helps.
const STALE: chrono::TimeDelta = chrono::TimeDelta::minutes(10);

/// Whether `at` on one of `days` came in `(prev, now]`.
pub fn passed(prev: NaiveDateTime, now: NaiveDateTime, at: NaiveTime, days: &[bool; 7]) -> bool {
    if now <= prev || now - prev > STALE {
        return false;
    }
    let mut date = prev.date();
    while date <= now.date() {
        let moment = date.and_time(at);
        if moment > prev && moment <= now && days[date.weekday().num_days_from_monday() as usize] {
            return true;
        }
        let Some(next) = date.succ_opt() else { break };
        date = next;
    }
    false
}

/// Starts and stops the queue at the scheduled times.
pub async fn run_scheduler(handle: AppHandle) {
    let mut prev = chrono::Local::now()
        .naive_local()
        .with_nanosecond(0)
        .unwrap_or_default();
    loop {
        tokio::time::sleep(Duration::from_secs(15)).await;
        let now = chrono::Local::now().naive_local();
        let app = handle.state::<App>();
        let schedule = lock(&app.settings).schedule.clone();
        if schedule.enabled {
            let fire =
                |t: &str| parse_time(t).is_some_and(|at| passed(prev, now, at, &schedule.days));
            if fire(&schedule.start) {
                tracing::info!("scheduler: starting the queue");
                app.manager.start_all().await.ok();
                let text =
                    crate::i18n::t(crate::lang(&handle), "Scheduler: started the queue", &[]);
                crate::to_page(&handle, "notice", text, false);
            }
            if fire(&schedule.stop) {
                tracing::info!("scheduler: stopping the queue");
                app.manager.pause_all().await.ok();
                let text = crate::i18n::t(crate::lang(&handle), "Scheduler: paused the queue", &[]);
                crate::to_page(&handle, "notice", text, false);
            }
        }
        prev = now;
    }
}

/// What to do once nothing is downloading or waiting any more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WhenDone {
    #[default]
    Nothing,
    Exit,
    Sleep,
    Hibernate,
    Shutdown,
}

impl WhenDone {
    /// The action in words, for notices.
    fn label(self) -> &'static str {
        match self {
            WhenDone::Nothing => "nothing",
            WhenDone::Exit => "close the app",
            WhenDone::Sleep => "sleep",
            WhenDone::Hibernate => "hibernate",
            WhenDone::Shutdown => "shut down",
        }
    }
}

/// How long the user has to stop the action.
pub const COUNTDOWN: u64 = 60;

/// The action chosen for when the downloads finish, and whether downloads ran since it was
/// chosen (so choosing it with an idle list does not shut the computer down at once).
#[derive(Default)]
pub struct Armed {
    pub action: WhenDone,
    pub saw_work: bool,
}

/// Each countdown gets a number; cancelling or a new countdown makes the old one stale.
static COUNTDOWN_ID: AtomicU64 = AtomicU64::new(0);

pub fn cancel_countdown() {
    COUNTDOWN_ID.fetch_add(1, Ordering::SeqCst);
}

/// Watches the list; when downloads ran since the action was chosen and none is left
/// downloading or queued, counts down and does it. One shot: the choice resets to nothing.
pub async fn run_when_done(handle: AppHandle) {
    let app = handle.state::<App>();
    let mut events = app.manager.subscribe();
    loop {
        match events.recv().await {
            Ok(Event::Updated(_) | Event::Removed(_) | Event::Added(_)) => {}
            Ok(Event::Progress(_) | Event::Moving(_)) => continue,
            Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => break,
        }
        // A file being copied to another drive counts too: sleeping would cut the copy.
        let busy = app.manager.is_moving()
            || app.manager.list().await.is_ok_and(|tasks| {
                tasks
                    .iter()
                    .any(|t| matches!(t.status, Status::Downloading | Status::Queued))
            });
        let action = {
            let mut armed = lock(&app.when_done);
            if armed.action == WhenDone::Nothing {
                continue;
            }
            if busy {
                armed.saw_work = true;
                continue;
            }
            if !armed.saw_work {
                continue;
            }
            let action = armed.action;
            *armed = Armed::default();
            action
        };
        handle.emit("when-done", WhenDone::Nothing).ok();
        countdown(&handle, action).await;
    }
}

/// Shows the countdown in the window; does the action unless it is cancelled in time.
async fn countdown(handle: &AppHandle, action: WhenDone) {
    let id = COUNTDOWN_ID.fetch_add(1, Ordering::SeqCst) + 1;
    crate::to_page(
        handle,
        "countdown",
        serde_json::json!({ "action": action, "seconds": COUNTDOWN }),
        true,
    );
    for _ in 0..COUNTDOWN {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if COUNTDOWN_ID.load(Ordering::SeqCst) != id {
            return;
        }
    }
    perform(handle, action).await;
}

/// Does `action` now. `SHARDLOAD_DRY_POWER` only reports it, for trying the feature out.
pub async fn perform(handle: &AppHandle, action: WhenDone) {
    tracing::info!("when done: {action:?}");
    let l = crate::lang(handle);
    let what = crate::i18n::t(l, action.label(), &[]);
    if std::env::var_os("SHARDLOAD_DRY_POWER").is_some() {
        let text = crate::i18n::t(
            l,
            "When done: would {action} now (dry run)",
            &[("action", &what)],
        );
        crate::to_page(handle, "notice", text, false);
        return;
    }
    let result = match action {
        WhenDone::Nothing => Ok(()),
        WhenDone::Exit => {
            handle.exit(0);
            Ok(())
        }
        WhenDone::Sleep => suspend(),
        WhenDone::Hibernate => power(Power::Hibernate),
        WhenDone::Shutdown => {
            // Closed first, so the list is saved before the system ends the app.
            handle.state::<App>().manager.close().await;
            power(Power::Off)
        }
    };
    if let Err(e) = result {
        let text = crate::i18n::t(
            l,
            "Could not {action}: {error}",
            &[("action", &what), ("error", &e)],
        );
        crate::to_page(handle, "notice", text, false);
    }
}

enum Power {
    Hibernate,
    Off,
}

/// Windows: `shutdown.exe`; elsewhere systemd's `systemctl`, which lets the user at the
/// seat do this without a password.
fn power(what: Power) -> Result<(), String> {
    let (program, args): (&str, &[&str]) = match (cfg!(windows), what) {
        (true, Power::Hibernate) => ("shutdown.exe", &["/h"]),
        (true, Power::Off) => ("shutdown.exe", &["/s", "/t", "0"]),
        (false, Power::Hibernate) => ("systemctl", &["hibernate"]),
        (false, Power::Off) => ("systemctl", &["poweroff"]),
    };
    std::process::Command::new(program)
        .args(args)
        .status()
        .map_err(|e| e.to_string())
        .and_then(|s| {
            s.success()
                .then_some(())
                .ok_or_else(|| format!("{program} {s}"))
        })
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn suspend() -> Result<(), String> {
    use windows_sys::Win32::System::Power::SetSuspendState;
    // Sleep, not hibernate; wake events stay allowed.
    // SAFETY: plain Win32 call without pointers.
    let ok = unsafe { SetSuspendState(false, false, false) };
    if ok {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

#[cfg(not(windows))]
fn suspend() -> Result<(), String> {
    std::process::Command::new("systemctl")
        .arg("suspend")
        .status()
        .map_err(|e| e.to_string())
        .and_then(|s| {
            s.success()
                .then_some(())
                .ok_or_else(|| format!("systemctl {s}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").expect("time")
    }

    #[test]
    fn fires_once_when_the_time_is_crossed() {
        let t = parse_time("02:00").expect("time");
        let all = [true; 7];
        assert!(passed(
            at("2026-10-17 01:59:50"),
            at("2026-10-17 02:00:05"),
            t,
            &all
        ));
        assert!(passed(
            at("2026-10-17 01:59:50"),
            at("2026-10-17 02:00:00"),
            t,
            &all
        ));
        // The next wake-up does not fire again.
        assert!(!passed(
            at("2026-10-17 02:00:05"),
            at("2026-10-17 02:00:20"),
            t,
            &all
        ));
        // Over midnight.
        let m = parse_time("00:00").expect("time");
        assert!(passed(
            at("2026-10-17 23:59:55"),
            at("2026-10-18 00:00:10"),
            m,
            &all
        ));
    }

    #[test]
    fn keeps_to_the_days_and_skips_stale_wake_ups() {
        let t = parse_time("02:00").expect("time");
        // 2026-10-17 is a Saturday.
        let mut weekdays = [true; 7];
        weekdays[5] = false;
        weekdays[6] = false;
        assert!(!passed(
            at("2026-10-17 01:59:50"),
            at("2026-10-17 02:00:05"),
            t,
            &weekdays
        ));
        assert!(passed(
            at("2026-10-19 01:59:50"),
            at("2026-10-19 02:00:05"),
            t,
            &weekdays
        ));
        // Woke up from sleep an hour later: not fired.
        assert!(!passed(
            at("2026-10-17 01:30:00"),
            at("2026-10-17 02:30:00"),
            t,
            &[true; 7]
        ));
    }

    #[test]
    fn times() {
        assert!(parse_time("7:05").is_some());
        assert!(parse_time("24:00").is_none());
        let bad = Schedule {
            start: "nine".into(),
            ..Schedule::default()
        };
        assert!(bad.validate().is_err());
        let empty = Schedule {
            enabled: true,
            start: String::new(),
            stop: String::new(),
            ..Schedule::default()
        };
        assert!(empty.validate().is_err());
    }
}
