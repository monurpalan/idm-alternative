//! The manager's events to the UI, notifications, the tray's tooltip, and the browser
//! extensions heard from.

use super::*;

/// How often the UI gets the progress of all running downloads, as one message.
pub(crate) const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// Sends manager events to the UI. List changes go out at once; progress is collected and
/// sent for all downloads together a few times a second, so the UI handles one message per
/// tick instead of one per download (with hundreds of rows, one each could outpace it).
/// Also shows a notification when a download finishes. `known` is the list as it was when
/// `events` was subscribed to.
pub(crate) async fn forward_events(
    handle: AppHandle,
    mut events: broadcast::Receiver<Event>,
    known: Vec<Task>,
) {
    let mut pending: HashMap<TaskId, TaskProgress> = HashMap::new();
    let mut statuses = Statuses::from_list(known.iter().map(|t| (t.id, t.status)));
    let mut tick = tokio::time::interval(PROGRESS_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks: u64 = 0;
    let mut shown_tip = String::from("Shardload");
    loop {
        tokio::select! {
            e = events.recv() => match e {
                Ok(Event::Progress(p)) => {
                    lock(&handle.state::<App>().live).insert(
                        p.id,
                        Live {
                            received: p.received,
                            speed: p.speed,
                            eta: p.eta,
                            hints: p.hints.clone(),
                        },
                    );
                    pending.insert(p.id, p);
                }
                Ok(e) => {
                    match &e {
                        Event::Added(t) => statuses.seen(t.id, t.status),
                        Event::Updated(t) => {
                            // A download that stopped must not get a stale progress afterwards.
                            if t.status != Status::Downloading {
                                pending.remove(&t.id);
                                lock(&handle.state::<App>().live).remove(&t.id);
                            }
                            let change = statuses.updated(t.id, t.status);
                            if change == Change::Started
                                && lock(&handle.state::<App>().settings).progress_windows
                            {
                                open_progress(&handle, t.id, false);
                            }
                            if change == Change::Finished {
                                notify_done(&handle, t);
                                scan_done(&handle, t);
                            }
                        }
                        Event::Removed(id) => {
                            pending.remove(id);
                            lock(&handle.state::<App>().live).remove(id);
                            statuses.removed(*id);
                        }
                        Event::DiskLow(low) => notify_disk_low(&handle, low),
                        Event::Budget(b) => notify_budget(&handle, b),
                        Event::DeadlineRisk(r) => notify_deadline(&handle, r),
                        Event::Progress(_) | Event::Moving(_) => {}
                    }
                    handle.emit("manager", &e).ok();
                }
                Err(RecvError::Lagged(_)) => {
                    handle.emit("manager-lagged", ()).ok();
                }
                Err(RecvError::Closed) => break,
            },
            _ = tick.tick() => {
                if !pending.is_empty() {
                    let batch: Vec<TaskProgress> = pending.drain().map(|(_, p)| p).collect();
                    handle.emit("manager-progress", &batch).ok();
                }
                // About once a second is enough for a tooltip.
                ticks += 1;
                if ticks.is_multiple_of(4) && let Some(tray) = handle.tray_by_id("main") {
                    let tip = tray_tooltip(&handle);
                    if tip != shown_tip {
                        tray.set_tooltip(Some(&tip)).ok();
                        shown_tip = tip;
                    }
                }
            }
        }
    }
}

/// The last status seen of each download, to notice the moment one starts or finishes.
#[derive(Debug, Default)]
pub(crate) struct Statuses {
    last: HashMap<TaskId, Status>,
    /// Listed as downloading, and not heard of since: the first "downloading" heard for one
    /// is still its start (its progress window opens then).
    listed_running: std::collections::HashSet<TaskId>,
}

/// What a download's new status means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    /// It began downloading.
    Started,
    /// It went from downloading to done.
    Finished,
    Other,
}

impl Statuses {
    /// Starts from the list as it is: the manager resumes downloads as it opens, before
    /// anyone listens, and those are already downloading.
    pub(crate) fn from_list(list: impl IntoIterator<Item = (TaskId, Status)>) -> Self {
        let last: HashMap<TaskId, Status> = list.into_iter().collect();
        let listed_running = last
            .iter()
            .filter(|(_, s)| **s == Status::Downloading)
            .map(|(id, _)| *id)
            .collect();
        Self {
            last,
            listed_running,
        }
    }

    pub(crate) fn seen(&mut self, id: TaskId, status: Status) {
        self.listed_running.remove(&id);
        self.last.insert(id, status);
    }

    pub(crate) fn updated(&mut self, id: TaskId, status: Status) -> Change {
        let unheard = self.listed_running.remove(&id);
        let before = self.last.insert(id, status);
        match (before, status) {
            (Some(Status::Downloading), Status::Downloading) if !unheard => Change::Other,
            (_, Status::Downloading) => Change::Started,
            (Some(Status::Downloading), Status::Done) => Change::Finished,
            _ => Change::Other,
        }
    }

    pub(crate) fn removed(&mut self, id: TaskId) {
        self.listed_running.remove(&id);
        self.last.remove(&id);
    }
}

/// Always said, whatever the notification setting: downloads stopped and wait for the user.
pub(crate) fn notify_disk_low(handle: &AppHandle, low: &shardload_manager::disk::DiskLow) {
    let lang = lang(handle);
    let body = i18n::t(
        lang,
        "{n} downloads paused: {volume} has {free} MB left. Free some space or move them, then resume.",
        &[
            ("n", &low.paused.len().to_string()),
            ("volume", &low.volume.display().to_string()),
            ("free", &(low.free >> 20).to_string()),
        ],
    );
    if let Err(e) = handle
        .notification()
        .builder()
        .title(i18n::t(lang, "The drive is almost full", &[]))
        .body(body)
        .show()
    {
        tracing::warn!("could not show a notification: {e}");
    }
}

/// The monthly data budget nearly or wholly used: said in the window and as a notification.
pub(crate) fn notify_budget(handle: &AppHandle, b: &shardload_manager::net::BudgetNotice) {
    let lang = lang(handle);
    let gb = |bytes: u64| i18n::decimal(lang, bytes as f64 / f64::from(1u32 << 30));
    let params = [("used", gb(b.used)), ("budget", gb(b.budget))];
    let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let text = if b.stopped {
        "This month's data budget is used up ({used} of {budget} GB): downloads wait until next month or a larger budget"
    } else if b.used >= b.budget {
        "This month's data budget is used up ({used} of {budget} GB)"
    } else {
        "{used} of this month's {budget} GB data budget used"
    };
    let body = i18n::t(lang, text, &params);
    to_page(handle, "notice", &body, false);
    if let Err(e) = handle
        .notification()
        .builder()
        .title(i18n::t(lang, "Data budget", &[]))
        .body(body)
        .show()
    {
        tracing::warn!("could not show a notification: {e}");
    }
}

/// A download that will not make its deadline at the speed now: said early, once.
pub(crate) fn notify_deadline(handle: &AppHandle, r: &shardload_manager::deadline::DeadlineRisk) {
    let lang = lang(handle);
    let name = r.name.clone().unwrap_or_else(|| format!("#{}", r.id));
    let mbps = |b: f64| i18n::decimal(lang, b * 8.0 / 1e6);
    let body = i18n::t(
        lang,
        "{name} will not be done in time at {speed} Mbps; it needs {needed} Mbps",
        &[
            ("name", &name),
            ("speed", &mbps(r.speed)),
            ("needed", &mbps(r.needed)),
        ],
    );
    to_page(handle, "notice", &body, false);
    if let Err(e) = handle
        .notification()
        .builder()
        .title(i18n::t(lang, "A deadline is at risk", &[]))
        .body(body)
        .show()
    {
        tracing::warn!("could not show a notification: {e}");
    }
}

pub(crate) fn notify_done(handle: &AppHandle, task: &Task) {
    if !lock(&handle.state::<App>().settings).notify {
        return;
    }
    let name = task.name.clone().unwrap_or_else(|| task.url.clone());
    if let Err(e) = handle
        .notification()
        .builder()
        .title(i18n::t(lang(handle), "Download complete", &[]))
        .body(name)
        .show()
    {
        tracing::warn!("could not show a notification: {e}");
    }
}

/// Scans a finished download when the settings say so; the user hears of anything but a
/// clean result.
pub(crate) fn scan_done(handle: &AppHandle, task: &Task) {
    let app = handle.state::<App>();
    let (on, scanner) = {
        let s = lock(&app.settings);
        (s.virus_scan, s.scanner.clone())
    };
    let Some(path) = task.path.clone().filter(|_| on) else {
        return;
    };
    let name = task.name.clone().unwrap_or_else(|| task.url.clone());
    let handle = handle.clone();
    tauri::async_runtime::spawn(async move {
        let l = lang(&handle);
        let text = match scan::scan(&path, &scanner).await {
            Ok(scan::Verdict::Clean) => {
                tracing::info!("virus scan of {}: clean", path.display());
                return;
            }
            Ok(scan::Verdict::Threat) => i18n::t(
                l,
                "Microsoft Defender found a threat in {name} and dealt with it",
                &[("name", &name)],
            ),
            Ok(scan::Verdict::Other(code)) => i18n::t(
                l,
                "The virus scanner reported a problem with {name} (exit code {code})",
                &[("name", &name), ("code", &code.to_string())],
            ),
            Err(e) => i18n::t(
                l,
                "Could not scan {name} for viruses: {error}",
                &[("name", &name), ("error", &e)],
            ),
        };
        tracing::warn!("{text}");
        handle
            .notification()
            .builder()
            .title(i18n::t(l, "Virus scan", &[]))
            .body(&text)
            .show()
            .ok();
        to_page(&handle, "notice", text, false);
    });
}

/// What the page shows about the browser extensions now.
pub(crate) fn extension_status_now(app: &App) -> extensions::Status {
    let integration = lock(&app.settings).browser_integration;
    let installed = shardload_bridge::installed_browsers();
    lock(&app.extensions).status(integration, &installed, extensions::now())
}

/// Changes the record of extensions and saves it; tells the page when what it shows changed.
pub(crate) async fn update_extensions(
    handle: &AppHandle,
    change: impl FnOnce(&mut extensions::Record),
) {
    let app = handle.state::<App>();
    let before = extension_status_now(&app);
    let record = {
        let mut r = lock(&app.extensions);
        change(&mut r);
        r.clone()
    };
    if let Err(e) = app.manager.save_setting(extensions::KEY, &record).await {
        tracing::warn!("could not save the extensions heard from: {e}");
    }
    let after = extension_status_now(&app);
    if (&after.browsers, &after.warning) != (&before.browsers, &before.warning) {
        handle.emit("extensions", &after).ok();
    }
}

/// The browser extensions heard from, and the warning about them, if any.
#[tauri::command]
pub(crate) fn extension_status(app: State<'_, App>) -> extensions::Status {
    extension_status_now(&app)
}

/// "Don't show again" on "Browser extension not found".
#[tauri::command]
pub(crate) async fn dismiss_extension_warning(handle: AppHandle) -> extensions::Status {
    update_extensions(&handle, |r| r.dismissed = true).await;
    extension_status_now(&handle.state::<App>())
}

/// Opens where to get the extension: for `browser`, or for any.
#[tauri::command]
pub(crate) fn install_extension(handle: AppHandle, browser: Option<String>) -> CmdResult<()> {
    let url = extensions::install_url(browser.as_deref());
    handle.opener().open_url(url, None::<&str>).map_err(msg)
}

/// A running download as last reported.
#[derive(Debug, Clone)]
pub(crate) struct Live {
    pub(crate) received: u64,
    pub(crate) speed: f64,
    /// Seconds left, `[soonest, latest]`; `None` while calculating.
    pub(crate) eta: Option<[f64; 2]>,
    /// What holds it back (see [`shardload_manager::hints`]).
    pub(crate) hints: Vec<shardload_manager::hints::Hint>,
}

/// The tray's tooltip: how many are downloading and when they will all be done.
pub(crate) fn tray_tooltip(handle: &AppHandle) -> String {
    let live: Vec<Live> = lock(&handle.state::<App>().live)
        .values()
        .cloned()
        .collect();
    if live.is_empty() {
        return "Shardload".into();
    }
    let lang = lang(handle);
    let n = live.len().to_string();
    // They run side by side: all are done when the last one is.
    let last = live
        .iter()
        .filter_map(|l| l.eta)
        .fold(None, |acc: Option<[f64; 2]>, [a, b]| {
            Some(acc.map_or([a, b], |[x, y]| [x.max(a), y.max(b)]))
        });
    let line = match last {
        Some([a, b]) => i18n::t(
            lang,
            "{n} downloading, {time} left",
            &[("n", &n), ("time", &i18n::time_left(lang, a, b))],
        ),
        None => i18n::t(lang, "{n} downloading", &[("n", &n)]),
    };
    format!("Shardload\n{line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_resumed_before_the_events_were_heard_is_seen_finishing() {
        // The manager resumed download 1 as it opened; its "downloading" went out before
        // anyone listened, and the list says so.
        let mut statuses = Statuses::from_list([(1, Status::Downloading), (2, Status::Done)]);
        assert_eq!(statuses.updated(1, Status::Done), Change::Finished);
        // One finished long ago and changed again (renamed, say) is not announced again.
        assert_eq!(statuses.updated(2, Status::Done), Change::Other);
    }

    #[test]
    fn a_download_listed_as_downloading_starts_when_first_heard_downloading() {
        // Resumed as the manager opened, or started by the queue between subscribing and
        // reading the list: its first "downloading" heard is its start, once.
        let mut statuses = Statuses::from_list([(1, Status::Downloading), (2, Status::Done)]);
        assert_eq!(statuses.updated(1, Status::Downloading), Change::Started);
        assert_eq!(statuses.updated(1, Status::Downloading), Change::Other);
        assert_eq!(statuses.updated(1, Status::Done), Change::Finished);
        // One listed as done is never announced, whatever comes next.
        assert_eq!(statuses.updated(2, Status::Done), Change::Other);
    }

    #[test]
    fn starts_and_finishes() {
        let mut statuses = Statuses::default();
        statuses.seen(1, Status::Queued);
        assert_eq!(statuses.updated(1, Status::Downloading), Change::Started);
        assert_eq!(statuses.updated(1, Status::Downloading), Change::Other);
        assert_eq!(statuses.updated(1, Status::Done), Change::Finished);
        assert_eq!(statuses.updated(1, Status::Downloading), Change::Started);
        assert_eq!(statuses.updated(1, Status::Paused), Change::Other);
        assert_eq!(statuses.updated(1, Status::Done), Change::Other);
        statuses.removed(1);
        assert_eq!(statuses.updated(1, Status::Done), Change::Other);
    }
}
