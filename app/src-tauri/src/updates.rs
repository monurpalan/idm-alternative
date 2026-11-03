//! The daily checks (the app's own update, the helper programs) and what the computer
//! does when all downloads finish.

use super::*;

/// How often the tools and the app are checked for new releases, and how long after start
/// the first check waits (so it does not compete with resuming downloads).
pub(crate) const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
pub(crate) const CHECK_DELAY: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether the check whose time is kept in `stamp` is due.
pub(crate) fn due(stamp: &Path) -> bool {
    std::fs::metadata(stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_none_or(|age| age >= CHECK_EVERY)
}

pub(crate) fn touch(stamp: &Path) {
    if let Some(dir) = stamp.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(stamp, b"").ok();
}

/// Once a day: keeps yt-dlp, deno and ffmpeg current while `update_tools` is on, never while a
/// download runs through them (Windows cannot replace a running program), and offers a new
/// version of the app while `check_updates` is on. The times of the last checks are kept in
/// files, so restarting the app does not check again.
pub(crate) async fn daily_checks(handle: AppHandle) {
    tokio::time::sleep(CHECK_DELAY).await;
    loop {
        let app = handle.state::<App>();
        let (update_tools, check_updates) = {
            let s = lock(&app.settings);
            (s.update_tools, s.check_updates)
        };

        let stamp = tools::dir(&app.data).join("last-check");
        if update_tools && due(&stamp) && !tools_busy(&app).await {
            update_tools_now(&handle, &app, &stamp).await;
        }

        let stamp = app.data.join("update-check");
        if check_updates && due(&stamp) {
            offer_update(&handle, &stamp).await;
        }
        // Wake up hourly: the app may run for days, and a busy or offline check retries soon.
        tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;
    }
}

/// Whether a download runs through yt-dlp or ffmpeg (pages, streams), which Windows cannot
/// replace while they run.
async fn tools_busy(app: &App) -> bool {
    app.manager.list().await.is_ok_and(|tasks| {
        tasks.iter().any(|t| {
            // yt-dlp for pages; ffmpeg for pages and streams.
            t.status == shardload_manager::Status::Downloading
                && t.stream
                    .as_ref()
                    .is_some_and(|s| s.torrent.is_none() && s.archive.is_none())
        })
    })
}

/// Updates yt-dlp, deno and ffmpeg (only when it is the one set up), and notes the check in
/// `stamp` unless one failed.
async fn update_tools_now(handle: &AppHandle, app: &App, stamp: &Path) {
    let opts = app.manager.config().engine;
    let mut changed = false;
    let mut failed = false;
    // ffmpeg only when it is the one set up (`update` skips tools not set up).
    for tool in [tools::Tool::Ytdlp, tools::Tool::Deno, tools::Tool::Ffmpeg] {
        match tools::update(&app.data, tool, &opts).await {
            Ok(Some(v)) => {
                tracing::info!("updated {tool:?} to {v}");
                changed = true;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("could not update {tool:?}: {e}");
                failed = true;
            }
        }
    }
    if changed {
        let settings = lock(&app.settings).clone();
        app.manager
            .set_config(manager_config(
                &settings,
                app.path_ffmpeg.clone(),
                &app.data,
            ))
            .await
            .ok();
        handle.emit("tools", ()).ok();
    }
    // A failed check (offline, GitHub busy) is tried again at the next wake-up.
    if !failed {
        touch(stamp);
    }
}

/// Offers a newer version of the app, if there is one; notes the check in `stamp`.
async fn offer_update(handle: &AppHandle, stamp: &Path) {
    match find_update(handle).await {
        Ok(found) => {
            if let Some(update) = found {
                to_page(handle, "update", update, false);
            }
            touch(stamp);
        }
        Err(e) => tracing::warn!("could not check for a new version: {e}"),
    }
}

/// A newer version of the app.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct AppUpdate {
    version: String,
    notes: Option<String>,
}

pub(crate) async fn find_update(handle: &AppHandle) -> Result<Option<AppUpdate>, String> {
    use tauri_plugin_updater::UpdaterExt as _;
    let update = handle.updater().map_err(msg)?.check().await.map_err(msg)?;
    Ok(update.map(|u| AppUpdate {
        version: u.version,
        notes: u.body,
    }))
}

/// Looks for a newer version of the app now.
#[tauri::command]
pub(crate) async fn check_update(handle: AppHandle) -> CmdResult<Option<AppUpdate>> {
    find_update(&handle).await
}

/// Downloads the newer version, closes the download list (running downloads keep their
/// progress and resume after the restart) and runs the installer, which ends this process
/// and starts the new version.
#[tauri::command]
pub(crate) async fn install_update(handle: AppHandle, app: State<'_, App>) -> CmdResult<()> {
    use tauri_plugin_updater::UpdaterExt as _;
    let update = handle
        .updater()
        .map_err(msg)?
        .check()
        .await
        .map_err(msg)?
        .ok_or("there is no newer version")?;
    let bytes = update.download(|_, _| {}, || {}).await.map_err(msg)?;
    app.manager.close().await;
    update
        .install(bytes)
        .map_err(|e| format!("{e}; restart the app to resume the downloads"))
}

/// Chooses what happens when all downloads finish. Downloads must run after the choice before
/// it fires, unless some are running or queued already.
#[tauri::command]
pub(crate) async fn set_when_done(
    handle: AppHandle,
    _app: State<'_, App>,
    action: schedule::WhenDone,
) -> CmdResult<()> {
    arm_when_done(&handle, action).await
}

/// Arms (or with `Nothing` disarms) the action for when every download is finished.
pub(crate) async fn arm_when_done(handle: &AppHandle, action: schedule::WhenDone) -> CmdResult<()> {
    let app = handle.state::<App>();
    let busy = app.manager.list().await.map_err(msg)?.iter().any(|t| {
        matches!(
            t.status,
            shardload_manager::Status::Downloading | shardload_manager::Status::Queued
        )
    });
    *lock(&app.when_done) = schedule::Armed {
        action,
        saw_work: busy,
    };
    handle.emit("when-done", action).ok();
    Ok(())
}

#[tauri::command]
pub(crate) fn get_when_done(app: State<'_, App>) -> schedule::WhenDone {
    lock(&app.when_done).action
}

/// Stops a running countdown, or does its action right away.
#[tauri::command]
pub(crate) async fn end_countdown(
    handle: AppHandle,
    now: Option<schedule::WhenDone>,
) -> CmdResult<()> {
    schedule::cancel_countdown();
    if let Some(action) = now {
        schedule::perform(&handle, action).await;
    }
    Ok(())
}
