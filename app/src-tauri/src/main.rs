//! Desktop app: a thin Tauri shell over `idma-manager`. Commands forward to the manager, and
//! its events go to the web UI as `manager` events (`manager-lagged` when some were dropped
//! and the UI should reload the list).

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clipboard;
mod i18n;
mod scan;
mod schedule;
mod settings;
mod tools;
mod tray;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use idma_manager::{Config, Event, Manager, Move, NewTask, Status, Task, TaskId, TaskProgress};
use serde::{Deserialize, Serialize};
use settings::Settings;
use tauri::{AppHandle, Emitter, Manager as _, RunEvent, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::broadcast::{self, error::RecvError};

struct App {
    manager: Manager,
    /// The app's data folder (the list, the bridge token, the host manifests).
    data: PathBuf,
    settings: Mutex<Settings>,
    tray: Mutex<Option<tray::Tray>>,
    /// ffmpeg, found on PATH at start: joins stream video and audio.
    ffmpeg: Option<PathBuf>,
    /// A download waiting for the browser to capture a fresh link to it ("refresh in browser"),
    /// and since when.
    awaiting_link: Mutex<Option<(TaskId, std::time::Instant)>>,
    /// Stops the site grab that is running, if any.
    grabbing: Mutex<Option<idma_engine::CancellationToken>>,
    /// What to do when all downloads finish (one shot).
    when_done: Mutex<schedule::Armed>,
    /// Events waiting for the page, which is closed while the app sits in the tray.
    page: Mutex<Page>,
    /// Latest bytes received and speed of each running download, for local tools asking
    /// over the bridge (the page gets the same through events).
    live: Mutex<HashMap<TaskId, (u64, f64)>>,
    /// Listening to the clipboard while the setting is on.
    #[cfg(windows)]
    clipboard: Mutex<Option<clipboard::Watcher>>,
}

/// How long "refresh in browser" waits for the user to start the download again.
const AWAIT_LINK: std::time::Duration = std::time::Duration::from_secs(600);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The language the interface uses now (see `i18n::resolve`).
fn lang(handle: &AppHandle) -> &'static str {
    i18n::resolve(&lock(&handle.state::<App>().settings).language)
}

/// The interface's language, "en" or "tr", for the page.
#[tauri::command]
fn ui_language(handle: AppHandle) -> &'static str {
    lang(&handle)
}

/// Errors reach the UI as their message.
type CmdResult<T> = Result<T, String>;

fn msg(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
async fn list_tasks(app: State<'_, App>) -> CmdResult<Vec<Task>> {
    app.manager.list().await.map_err(msg)
}

/// What the "Add URL" dialog shows before the user decides.
#[derive(Serialize, Default)]
struct ProbeView {
    url: String,
    name: String,
    size: Option<u64>,
    resumable: bool,
    /// The server asks for a user name and password first.
    needs_login: bool,
    realm: Option<String>,
    /// The login saved for the site answered it: its user name.
    saved_login: Option<String>,
}

/// HTTP Basic credentials typed by the user.
#[derive(Deserialize)]
struct Login {
    user: String,
    password: String,
    /// Save it for the site, so its downloads log in by themselves.
    #[serde(default)]
    remember: bool,
}

/// Saves `login` for the site of `url` when the user asked to.
async fn remember_login(app: &App, url: &str, login: Option<&Login>) -> CmdResult<()> {
    let Some(login) = login.filter(|l| l.remember) else {
        return Ok(());
    };
    let site = idma_manager::site_of(url).ok_or("this address has no site to remember")?;
    app.manager
        .save_login(&site, &login.user, &login.password)
        .await
        .map_err(msg)
}

fn login_headers(login: Option<&Login>) -> Vec<(String, String)> {
    login
        .map(|l| idma_engine::basic_auth(&l.user, &l.password))
        .into_iter()
        .collect()
}

/// Headers the request carries: those the browser handed over, plus any login.
fn request_headers(
    headers: Option<Vec<(String, String)>>,
    login: Option<&Login>,
) -> Vec<(String, String)> {
    let mut all = headers.unwrap_or_default();
    if login.is_some() {
        all.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
        all.extend(login_headers(login));
    }
    all
}

#[tauri::command]
async fn probe(
    app: State<'_, App>,
    url: String,
    login: Option<Login>,
    headers: Option<Vec<(String, String)>>,
) -> CmdResult<ProbeView> {
    let view = |p: idma_engine::Probe, saved_login| ProbeView {
        url: p.url,
        name: p.name,
        size: p.size,
        resumable: p.resumable,
        saved_login,
        ..ProbeView::default()
    };
    match app
        .manager
        .probe(&url, &request_headers(headers.clone(), login.as_ref()))
        .await
    {
        Ok(p) => Ok(view(p, None)),
        Err(idma_manager::Error::Engine(idma_engine::Error::AuthRequired { realm })) => {
            // A saved login for the site: the download will use it, so ask with it too.
            if login.is_none()
                && let Ok(Some((user, password))) = app.manager.saved_login(&url).await
            {
                let saved = Login {
                    user: user.clone(),
                    password,
                    remember: false,
                };
                if let Ok(p) = app
                    .manager
                    .probe(&url, &request_headers(headers, Some(&saved)))
                    .await
                {
                    return Ok(view(p, Some(user)));
                }
            }
            Ok(ProbeView {
                url,
                needs_login: true,
                realm,
                ..ProbeView::default()
            })
        }
        Err(e) => Err(msg(e)),
    }
}

#[derive(Deserialize)]
struct AddRequest {
    url: String,
    name: Option<String>,
    dir: Option<PathBuf>,
    start: bool,
    login: Option<Login>,
    /// From the browser: cookies, referrer, user agent.
    headers: Option<Vec<(String, String)>>,
    /// A stream: `url` is then the chosen video playlist.
    stream: Option<idma_manager::Stream>,
    /// Other addresses of the same file.
    #[serde(default)]
    mirrors: Vec<String>,
}

#[tauri::command]
async fn add_task(app: State<'_, App>, request: AddRequest) -> CmdResult<Task> {
    remember_login(&app, &request.url, request.login.as_ref()).await?;
    app.manager
        .add(NewTask {
            headers: request_headers(request.headers, request.login.as_ref()),
            url: request.url,
            name: request.name.filter(|n| !n.trim().is_empty()),
            dir: request.dir,
            start: request.start,
            stream: request.stream,
            mirrors: clean_mirrors(request.mirrors),
            ..NewTask::default()
        })
        .await
        .map_err(msg)
}

/// Mirrors as typed or pasted: trimmed, empty lines and repeats left out.
fn clean_mirrors(mirrors: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in mirrors.iter().map(|m| m.trim()).filter(|m| !m.is_empty()) {
        if !out.iter().any(|o| o == m) {
            out.push(m.to_owned());
        }
    }
    out
}

/// A batch address (`photo[001-100].jpg`): how many downloads it stands for, and the first
/// and last. None for an ordinary address.
#[derive(Serialize)]
struct BatchView {
    count: usize,
    first: String,
    last: String,
}

#[tauri::command]
fn batch_preview(url: String) -> CmdResult<Option<BatchView>> {
    if !idma_manager::batch::is_pattern(&url) {
        return Ok(None);
    }
    let urls = idma_manager::batch::expand(&url)?;
    Ok(Some(BatchView {
        count: urls.len(),
        first: urls.first().cloned().unwrap_or_default(),
        last: urls.last().cloned().unwrap_or_default(),
    }))
}

/// Adds every download a batch address stands for, each named by its server.
#[tauri::command]
async fn add_batch(app: State<'_, App>, request: AddRequest) -> CmdResult<usize> {
    let urls = idma_manager::batch::expand(&request.url)?;
    remember_login(&app, &request.url, request.login.as_ref()).await?;
    let headers = request_headers(request.headers, request.login.as_ref());
    for url in &urls {
        app.manager
            .add(NewTask {
                url: url.clone(),
                headers: headers.clone(),
                dir: request.dir.clone(),
                start: request.start,
                ..NewTask::default()
            })
            .await
            .map_err(msg)?;
    }
    Ok(urls.len())
}

/// Credentials for a download whose server asked for them; it continues with them.
#[tauri::command]
async fn set_login(app: State<'_, App>, id: TaskId, login: Login) -> CmdResult<()> {
    let task = app.manager.get(id).await.map_err(msg)?;
    remember_login(&app, &task.url, Some(&login)).await?;
    app.manager
        .set_login(id, &login.user, &login.password)
        .await
        .map_err(msg)
}

/// Sites with a remembered login, for the settings.
#[tauri::command]
async fn saved_logins(app: State<'_, App>) -> CmdResult<Vec<idma_manager::SavedLogin>> {
    app.manager.saved_logins().await.map_err(msg)
}

#[tauri::command]
async fn forget_login(app: State<'_, App>, site: String) -> CmdResult<()> {
    app.manager.forget_login(&site).await.map_err(msg)
}

#[tauri::command]
async fn start_tasks(app: State<'_, App>, ids: Vec<TaskId>) -> CmdResult<()> {
    for id in ids {
        app.manager.start(id).await.map_err(msg)?;
    }
    Ok(())
}

#[tauri::command]
async fn pause_tasks(app: State<'_, App>, ids: Vec<TaskId>) -> CmdResult<()> {
    for id in ids {
        app.manager.pause(id).await.map_err(msg)?;
    }
    Ok(())
}

#[tauri::command]
async fn remove_tasks(app: State<'_, App>, ids: Vec<TaskId>, delete_files: bool) -> CmdResult<()> {
    for id in ids {
        app.manager.remove(id, delete_files).await.map_err(msg)?;
    }
    Ok(())
}

#[tauri::command]
async fn start_all(app: State<'_, App>) -> CmdResult<()> {
    app.manager.start_all().await.map_err(msg)
}

#[tauri::command]
async fn pause_all(app: State<'_, App>) -> CmdResult<()> {
    app.manager.pause_all().await.map_err(msg)
}

#[tauri::command]
async fn move_task(app: State<'_, App>, id: TaskId, to: Move) -> CmdResult<()> {
    app.manager.move_task(id, to).await.map_err(msg)
}

#[tauri::command]
async fn restart_task(app: State<'_, App>, id: TaskId) -> CmdResult<()> {
    app.manager.restart(id).await.map_err(msg)
}

/// This download's own limit in bytes per second; `None` removes it.
#[tauri::command]
async fn set_speed_limit(app: State<'_, App>, id: TaskId, rate: Option<u64>) -> CmdResult<()> {
    app.manager
        .set_speed_limit(id, rate.filter(|r| *r > 0))
        .await
        .map_err(msg)
}

/// Opens the page the download came from (or its address) in the default browser and waits
/// for the extension to capture the download again: the captured link then continues this
/// download instead of starting a new one.
#[tauri::command]
async fn refresh_in_browser(handle: AppHandle, app: State<'_, App>, id: TaskId) -> CmdResult<()> {
    let task = app.manager.get(id).await.map_err(msg)?;
    let page = task
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("referer"))
        .map_or_else(|| task.url.clone(), |(_, v)| v.clone());
    *lock(&app.awaiting_link) = Some((id, std::time::Instant::now()));
    let opened = handle.opener().open_url(page, None::<&str>);
    if opened.is_err() {
        // Nothing will come back from a browser that did not open: let the next capture be
        // a new download.
        lock(&app.awaiting_link).take();
    }
    opened.map_err(msg)
}

/// A magnet link or `.torrent` file the app was started with (Windows passes it when the
/// user opens one and the app handles them); a relative path is taken from `cwd`.
fn torrent_arg(args: &[String], cwd: &Path) -> Option<String> {
    args.iter().skip(1).find_map(|a| {
        if a.to_ascii_lowercase().starts_with("magnet:") {
            return Some(a.clone());
        }
        let path = cwd.join(a);
        (idma_manager::torrent::is_torrent(a) && path.is_file())
            .then(|| path.to_string_lossy().into_owned())
    })
}

/// Opens "Add download" with the torrent, as for a download from the browser.
fn offer_torrent(handle: &AppHandle, url: &str) {
    to_page(
        handle,
        "capture",
        serde_json::json!({ "url": url, "filename": null, "size": null, "headers": null }),
        true,
    );
}

/// Site grabber: the files linked from `url` and the pages below it, to `depth`. Progress
/// goes to the page as `grab-progress`; a new grab or `cancel_grab` stops a running one.
#[tauri::command]
async fn grab_site(
    handle: AppHandle,
    app: State<'_, App>,
    url: String,
    depth: u32,
    same_site: bool,
) -> CmdResult<Vec<idma_manager::grabber::Found>> {
    let cancel = idma_engine::CancellationToken::new();
    if let Some(old) = lock(&app.grabbing).replace(cancel.clone()) {
        old.cancel();
    }
    let opts = idma_manager::grabber::GrabOptions {
        depth: depth.clamp(1, 5),
        same_site,
        ..Default::default()
    };
    let progress = |p: idma_manager::grabber::GrabProgress| {
        handle.emit("grab-progress", p).ok();
    };
    let result = app.manager.grab_site(&url, &opts, progress, &cancel).await;
    lock(&app.grabbing).take();
    result.map_err(msg)
}

#[tauri::command]
fn cancel_grab(app: State<'_, App>) {
    if let Some(c) = lock(&app.grabbing).take() {
        c.cancel();
    }
}

/// A torrent's name and files (a magnet link asks peers for them), for picking files.
#[tauri::command]
async fn inspect_torrent(app: State<'_, App>, url: String) -> CmdResult<idma_manager::TorrentInfo> {
    app.manager.inspect_torrent(&url).await.map_err(msg)
}

/// What an HLS or DASH stream offers (qualities, audio, subtitles), read with the page's
/// headers.
#[tauri::command]
async fn inspect_media(
    app: State<'_, App>,
    url: String,
    headers: Option<Vec<(String, String)>>,
) -> CmdResult<idma_media::Streams> {
    let opts = app.manager.config().engine;
    let (text, final_url) =
        idma_engine::fetch_text(&url, &headers.unwrap_or_default(), &opts, 8 << 20)
            .await
            .map_err(msg)?;
    idma_media::parse(&final_url, &text).map_err(msg)
}

/// Continues a download from a fresh link to the same file.
#[tauri::command]
async fn refresh_link(app: State<'_, App>, id: TaskId, url: String) -> CmdResult<()> {
    app.manager
        .refresh_link(id, url.trim(), None)
        .await
        .map_err(|e| match e {
            idma_manager::Error::Engine(idma_engine::Error::FileChanged) => {
                "this link leads to a different file (size or content differ)".to_owned()
            }
            e => e.to_string(),
        })
}

/// The file of a download, looked up here so the UI never names arbitrary paths to open.
async fn file_of(app: &App, id: TaskId) -> CmdResult<PathBuf> {
    let task = app.manager.get(id).await.map_err(msg)?;
    task.path
        .ok_or_else(|| "this download has no file yet".to_owned())
}

#[tauri::command]
async fn open_file(handle: tauri::AppHandle, app: State<'_, App>, id: TaskId) -> CmdResult<()> {
    let path = file_of(&app, id).await?;
    handle
        .opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(msg)
}

#[tauri::command]
async fn show_in_folder(
    handle: tauri::AppHandle,
    app: State<'_, App>,
    id: TaskId,
) -> CmdResult<()> {
    let path = file_of(&app, id).await?;
    // An unfinished download only has its partial file on disk.
    let shown = if path.exists() {
        path
    } else {
        let mut part = path.clone().into_os_string();
        part.push(".idma-part");
        let part = PathBuf::from(part);
        if part.exists() {
            part
        } else {
            return handle
                .opener()
                .open_path(
                    path.parent().unwrap_or(&path).to_string_lossy(),
                    None::<&str>,
                )
                .map_err(msg);
        }
    };
    handle.opener().reveal_item_in_dir(shown).map_err(msg)
}

#[tauri::command]
fn get_settings(app: State<'_, App>) -> Settings {
    lock(&app.settings).clone()
}

#[tauri::command]
async fn set_settings(handle: AppHandle, settings: Settings) -> CmdResult<Settings> {
    apply_settings(&handle, settings).await
}

/// Checks, applies and saves new settings, and tells the UI and the tray.
async fn apply_settings(handle: &AppHandle, new: Settings) -> CmdResult<Settings> {
    new.validate()?;
    let app = handle.state::<App>();
    let old = lock(&app.settings).clone();
    app.manager
        .set_config(manager_config(&new, app.ffmpeg.clone(), &app.data))
        .await
        .map_err(msg)?;
    app.manager.set_global_limit(new.speed_limit);
    if new.browser_integration != old.browser_integration {
        set_browser_integration(&app.data, new.browser_integration)?;
    }
    if new.watch_clipboard != old.watch_clipboard {
        watch_clipboard(handle, new.watch_clipboard);
    }
    if new.torrent_links != old.torrent_links {
        let exe = std::env::current_exe().map_err(msg)?;
        if new.torrent_links {
            idma_bridge::register_torrent_links(&exe, idma_bridge::CLASSES)
        } else {
            idma_bridge::unregister_torrent_links(&exe, idma_bridge::CLASSES)
        }
        .map_err(|e| format!("could not change what opens magnet links: {e}"))?;
    }
    if new.start_with_windows != old.start_with_windows {
        let autostart = handle.autolaunch();
        if new.start_with_windows {
            autostart.enable()
        } else {
            autostart.disable()
        }
        .map_err(|e| format!("could not change starting with Windows: {e}"))?;
    }
    app.manager
        .save_setting(settings::KEY, &new)
        .await
        .map_err(msg)?;
    *lock(&app.settings) = new.clone();
    if new.language != old.language {
        match tray::build(handle, new.speed_limit, i18n::resolve(&new.language)) {
            Ok(t) => *lock(&app.tray) = Some(t),
            Err(e) => tracing::warn!("could not rebuild the tray menu: {e}"),
        }
    } else if let Some(t) = lock(&app.tray).as_ref() {
        t.show_limit(new.speed_limit);
    }
    handle.emit("settings", &new).ok();
    Ok(new)
}

/// Starts or stops listening to the clipboard. A copied download link (one not on the list
/// yet, and not the same as the last one) brings the window up with "Add URL" for it.
fn watch_clipboard(handle: &AppHandle, on: bool) {
    #[cfg(windows)]
    {
        let app = handle.state::<App>();
        let mut slot = lock(&app.clipboard);
        if !on {
            // Dropping the watcher stops it.
            slot.take();
            return;
        }
        if slot.is_some() {
            return;
        }
        let handle = handle.clone();
        let last = Mutex::new(String::new());
        let started = clipboard::Watcher::start(move |text| {
            let Some(url) = clipboard::link_in(&text) else {
                return;
            };
            if std::mem::replace(&mut *lock(&last), url.clone()) == url {
                return;
            }
            let handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                let app = handle.state::<App>();
                let listed = app
                    .manager
                    .list()
                    .await
                    .is_ok_and(|tasks| tasks.iter().any(|t| t.url == url));
                if !listed {
                    to_page(&handle, "clipboard", url, true);
                }
            });
        });
        match started {
            Ok(w) => *slot = Some(w),
            Err(e) => tracing::warn!("could not watch the clipboard: {e}"),
        }
    }
    #[cfg(not(windows))]
    let _ = (handle, on);
}

/// Registers the native messaging host with the browsers, or removes the registration.
///
/// The host is installed next to the app. A profile started with `IDMA_HOME` registers a
/// small script that starts the host with the same `IDMA_HOME`, so browsers reach that
/// profile and not the real one.
fn set_browser_integration(data: &Path, on: bool) -> CmdResult<()> {
    if !on {
        return idma_bridge::unregister(data).map_err(msg);
    }
    let exe = std::env::current_exe().map_err(msg)?;
    let host = exe.with_file_name(if cfg!(windows) {
        "idma-host.exe"
    } else {
        "idma-host"
    });
    if !host.exists() {
        return Err(format!(
            "the browser host is missing next to the app ({})",
            host.display()
        ));
    }
    let target = if std::env::var_os("IDMA_HOME").is_some() {
        let (script, text) = if cfg!(windows) {
            (
                data.join("idma-host.cmd"),
                format!(
                    "@echo off\nset \"IDMA_HOME={}\"\nset \"IDMA_APP={}\"\n\"{}\" %*\n",
                    data.display(),
                    exe.display(),
                    host.display()
                ),
            )
        } else {
            (
                data.join("idma-host.sh"),
                format!(
                    "#!/bin/sh\nexport IDMA_HOME='{}'\nexport IDMA_APP='{}'\nexec '{}' \"$@\"\n",
                    data.display(),
                    exe.display(),
                    host.display()
                ),
            )
        };
        std::fs::write(&script, text).map_err(msg)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .map_err(msg)?;
        }
        script
    } else {
        host
    };
    idma_bridge::register(&target, data).map_err(msg)
}

/// The manager's configuration: the settings plus the helper programs the app has.
fn manager_config(settings: &Settings, ffmpeg: Option<PathBuf>, data: &Path) -> Config {
    Config {
        ffmpeg,
        ytdlp: tools::installed(data, tools::Tool::Ytdlp),
        deno: tools::installed(data, tools::Tool::Deno),
        ..settings.config()
    }
}

/// What a page of a site such as YouTube offers, read by yt-dlp with the page's headers.
#[tauri::command]
async fn inspect_page(
    app: State<'_, App>,
    url: String,
    headers: Option<Vec<(String, String)>>,
) -> CmdResult<idma_manager::ytdlp::PageInfo> {
    let ytdlp = tools::installed(&app.data, tools::Tool::Ytdlp)
        .ok_or_else(|| "yt-dlp is not set up".to_owned())?;
    let tools = idma_manager::ytdlp::Tools {
        ytdlp,
        deno: tools::installed(&app.data, tools::Tool::Deno),
        ffmpeg: app.ffmpeg.clone(),
    };
    idma_manager::ytdlp::inspect(
        &tools,
        &url,
        &headers.unwrap_or_default(),
        &app.data.join("tools").join("scratch"),
    )
    .await
}

/// The first `name` in a folder on PATH.
fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Which helper programs the app has, and their versions.
#[derive(Serialize)]
struct Tools {
    ffmpeg: bool,
    ytdlp: Option<String>,
    deno: Option<String>,
}

#[tauri::command]
async fn tools(app: State<'_, App>) -> CmdResult<Tools> {
    let data = app.data.clone();
    let version = |tool| {
        let data = data.clone();
        async move {
            match tools::installed(&data, tool) {
                Some(exe) => tools::version(&exe).await,
                None => None,
            }
        }
    };
    Ok(Tools {
        ffmpeg: app.ffmpeg.is_some(),
        ytdlp: version(tools::Tool::Ytdlp).await,
        deno: version(tools::Tool::Deno).await,
    })
}

/// Sets up (or updates to the latest release) a helper program, on the user's request.
#[tauri::command]
async fn install_tool(app: State<'_, App>, tool: tools::Tool) -> CmdResult<String> {
    let opts = app.manager.config().engine;
    let exe = tools::install(&app.data, tool, &opts).await?;
    // Downloads started from now on use it.
    let settings = lock(&app.settings).clone();
    app.manager
        .set_config(manager_config(&settings, app.ffmpeg.clone(), &app.data))
        .await
        .map_err(msg)?;
    Ok(tools::version(&exe).await.unwrap_or_default())
}

/// How often the tools and the app are checked for new releases, and how long after start
/// the first check waits (so it does not compete with resuming downloads).
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
const CHECK_DELAY: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether the check whose time is kept in `stamp` is due.
fn due(stamp: &Path) -> bool {
    std::fs::metadata(stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_none_or(|age| age >= CHECK_EVERY)
}

fn touch(stamp: &Path) {
    if let Some(dir) = stamp.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(stamp, b"").ok();
}

/// Once a day: keeps yt-dlp and deno current while `update_tools` is on, never while a
/// download runs through them (Windows cannot replace a running program), and offers a new
/// version of the app while `check_updates` is on. The times of the last checks are kept in
/// files, so restarting the app does not check again.
async fn daily_checks(handle: AppHandle) {
    tokio::time::sleep(CHECK_DELAY).await;
    loop {
        let app = handle.state::<App>();
        let (update_tools, check_updates) = {
            let s = lock(&app.settings);
            (s.update_tools, s.check_updates)
        };

        let stamp = tools::dir(&app.data).join("last-check");
        let busy = app.manager.list().await.is_ok_and(|tasks| {
            tasks.iter().any(|t| {
                t.status == idma_manager::Status::Downloading
                    && t.stream.as_ref().is_some_and(|s| s.ytdlp.is_some())
            })
        });
        if update_tools && due(&stamp) && !busy {
            let opts = app.manager.config().engine;
            let mut changed = false;
            let mut failed = false;
            for tool in [tools::Tool::Ytdlp, tools::Tool::Deno] {
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
                    .set_config(manager_config(&settings, app.ffmpeg.clone(), &app.data))
                    .await
                    .ok();
                handle.emit("tools", ()).ok();
            }
            // A failed check (offline, GitHub busy) is tried again at the next wake-up.
            if !failed {
                touch(&stamp);
            }
        }

        let stamp = app.data.join("update-check");
        if check_updates && due(&stamp) {
            match find_update(&handle).await {
                Ok(found) => {
                    if let Some(update) = found {
                        to_page(&handle, "update", update, false);
                    }
                    touch(&stamp);
                }
                Err(e) => tracing::warn!("could not check for a new version: {e}"),
            }
        }
        // Wake up hourly: the app may run for days, and a busy or offline check retries soon.
        tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;
    }
}

/// A newer version of the app.
#[derive(Debug, Clone, serde::Serialize)]
struct AppUpdate {
    version: String,
    notes: Option<String>,
}

async fn find_update(handle: &AppHandle) -> Result<Option<AppUpdate>, String> {
    use tauri_plugin_updater::UpdaterExt as _;
    let update = handle.updater().map_err(msg)?.check().await.map_err(msg)?;
    Ok(update.map(|u| AppUpdate {
        version: u.version,
        notes: u.body,
    }))
}

/// Looks for a newer version of the app now.
#[tauri::command]
async fn check_update(handle: AppHandle) -> CmdResult<Option<AppUpdate>> {
    find_update(&handle).await
}

/// Downloads the newer version, closes the download list (running downloads keep their
/// progress and resume after the restart) and runs the installer, which ends this process
/// and starts the new version.
#[tauri::command]
async fn install_update(handle: AppHandle, app: State<'_, App>) -> CmdResult<()> {
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
async fn set_when_done(
    handle: AppHandle,
    app: State<'_, App>,
    action: schedule::WhenDone,
) -> CmdResult<()> {
    let busy = app.manager.list().await.map_err(msg)?.iter().any(|t| {
        matches!(
            t.status,
            idma_manager::Status::Downloading | idma_manager::Status::Queued
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
fn get_when_done(app: State<'_, App>) -> schedule::WhenDone {
    lock(&app.when_done).action
}

/// Stops a running countdown, or does its action right away.
#[tauri::command]
async fn end_countdown(handle: AppHandle, now: Option<schedule::WhenDone>) -> CmdResult<()> {
    schedule::cancel_countdown();
    if let Some(action) = now {
        schedule::perform(&handle, action).await;
    }
    Ok(())
}

/// Brings the window back, also from the tray.
///
/// In the tray the window does not exist (its WebView would keep 160 MB for nothing), so it
/// is built again here; the page then asks for what it missed (`page_ready`).
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        w.show().ok();
        w.unminimize().ok();
        w.set_focus().ok();
        return;
    }
    let Some(config) = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .cloned()
    else {
        return;
    };
    // Built off the main thread: building a window in the tray or single-instance callbacks,
    // which run on it, would deadlock on Windows.
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if app.get_webview_window("main").is_some() {
            return;
        }
        match tauri::WebviewWindowBuilder::from_config(&app, &config).and_then(|b| b.build()) {
            Ok(w) => {
                w.set_focus().ok();
            }
            Err(e) => tracing::error!("could not open the window: {e}"),
        }
    });
}

/// At most this many progress windows open by themselves: starting a long queue must not
/// cover the screen.
const MAX_PROGRESS_WINDOWS: usize = 4;

/// The small progress window of download `id` (see `routes/progress`). `asked`: the user
/// asked for it, so the limit does not apply.
fn open_progress(handle: &AppHandle, id: TaskId, asked: bool) {
    let label = format!("progress-{id}");
    if let Some(w) = handle.get_webview_window(&label) {
        w.show().ok();
        w.set_focus().ok();
        return;
    }
    let open = handle
        .webview_windows()
        .keys()
        .filter(|l| l.starts_with("progress-"))
        .count();
    if !asked && open >= MAX_PROGRESS_WINDOWS {
        return;
    }
    let handle = handle.clone();
    // Not on the main thread, for the same reason as `show_main`.
    tauri::async_runtime::spawn(async move {
        let built = tauri::WebviewWindowBuilder::new(
            &handle,
            &label,
            tauri::WebviewUrl::App(format!("progress?id={id}").into()),
        )
        .title(i18n::t(lang(&handle), "Download", &[]))
        .inner_size(440.0, 236.0)
        .resizable(false)
        .maximizable(false)
        .build();
        if let Err(e) = built {
            tracing::warn!("could not open the progress window: {e}");
        }
    });
}

/// Opens the progress window of a download from the list.
#[tauri::command]
fn show_progress(handle: AppHandle, id: TaskId) {
    open_progress(&handle, id, true);
}

/// Events the page must not miss while it is closed or still loading.
#[derive(Default)]
struct Page {
    ready: bool,
    waiting: Vec<(&'static str, serde_json::Value)>,
}

/// Sends `event` to the page now if it is open, otherwise when it has loaded; `show` brings
/// the window up. For what the user has to see (a captured download, a countdown, a notice);
/// the list and the settings the page reads afresh when it opens.
fn to_page(handle: &AppHandle, event: &'static str, payload: impl Serialize, show: bool) {
    let payload = serde_json::to_value(payload).unwrap_or_default();
    let app = handle.state::<App>();
    let ready = {
        let mut page = lock(&app.page);
        if !page.ready {
            // Days in the tray must not pile up notices without end.
            if page.waiting.len() >= 20 {
                page.waiting.remove(0);
            }
            page.waiting.push((event, payload.clone()));
        }
        page.ready
    };
    if ready {
        handle.emit(event, payload).ok();
    }
    if show {
        show_main(handle);
    }
}

/// The page has loaded and listens: hands it what came while it was closed.
#[tauri::command]
fn page_ready(handle: AppHandle, app: State<'_, App>) {
    let waiting = {
        let mut page = lock(&app.page);
        page.ready = true;
        std::mem::take(&mut page.waiting)
    };
    for (event, payload) in waiting {
        handle.emit(event, payload).ok();
    }
}

fn on_tray_menu(app: &AppHandle, id: &str) {
    let manager = app.state::<App>().manager.clone();
    match id {
        tray::SHOW => show_main(app),
        tray::START_ALL => {
            tauri::async_runtime::spawn(async move { manager.start_all().await.ok() });
        }
        tray::PAUSE_ALL => {
            tauri::async_runtime::spawn(async move { manager.pause_all().await.ok() });
        }
        tray::QUIT => app.exit(0),
        id => {
            let Some((limit, _)) = id
                .strip_prefix(tray::LIMIT_PREFIX)
                .and_then(|i| i.parse::<usize>().ok())
                .and_then(|i| tray::LIMITS.get(i))
            else {
                return;
            };
            let settings = Settings {
                speed_limit: *limit,
                ..lock(&app.state::<App>().settings).clone()
            };
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = apply_settings(&handle, settings).await {
                    tracing::warn!("could not set the speed limit: {e}");
                }
            });
        }
    }
}

/// How often the UI gets the progress of all running downloads, as one message.
const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// Sends manager events to the UI. List changes go out at once; progress is collected and
/// sent for all downloads together a few times a second, so the UI handles one message per
/// tick instead of one per download (with hundreds of rows, one each could outpace it).
/// Also shows a notification when a download finishes.
async fn forward_events(handle: AppHandle, mut events: broadcast::Receiver<Event>) {
    let mut pending: HashMap<TaskId, TaskProgress> = HashMap::new();
    // Last status seen per download, to notice the moment one finishes.
    let mut statuses: HashMap<TaskId, Status> = HashMap::new();
    let mut tick = tokio::time::interval(PROGRESS_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            e = events.recv() => match e {
                Ok(Event::Progress(p)) => {
                    lock(&handle.state::<App>().live).insert(p.id, (p.received, p.speed));
                    pending.insert(p.id, p);
                }
                Ok(e) => {
                    match &e {
                        Event::Added(t) => {
                            statuses.insert(t.id, t.status);
                        }
                        Event::Updated(t) => {
                            // A download that stopped must not get a stale progress afterwards.
                            if t.status != Status::Downloading {
                                pending.remove(&t.id);
                                lock(&handle.state::<App>().live).remove(&t.id);
                            }
                            let before = statuses.insert(t.id, t.status);
                            if t.status == Status::Downloading
                                && before != Some(Status::Downloading)
                                && lock(&handle.state::<App>().settings).progress_windows
                            {
                                open_progress(&handle, t.id, false);
                            }
                            if t.status == Status::Done && before == Some(Status::Downloading) {
                                notify_done(&handle, t);
                                scan_done(&handle, t);
                            }
                        }
                        Event::Removed(id) => {
                            pending.remove(id);
                            lock(&handle.state::<App>().live).remove(id);
                            statuses.remove(id);
                        }
                        Event::Progress(_) => {}
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
            }
        }
    }
}

fn notify_done(handle: &AppHandle, task: &Task) {
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
fn scan_done(handle: &AppHandle, task: &Task) {
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

/// Answers the browser extension (through the native host): pings, and downloads or link
/// lists it hands over, which open in the window for the user to confirm.
async fn bridge_reply(handle: &AppHandle, request: idma_bridge::Request) -> idma_bridge::Reply {
    use idma_bridge::{Reply, Request};
    match request {
        Request::Ping => Reply {
            version: Some(env!("CARGO_PKG_VERSION").into()),
            ..Reply::ok()
        },
        Request::Download(capture) => {
            // A download waiting for a fresh link takes this one if it is the same file.
            let waiting = {
                let app = handle.state::<App>();
                let mut awaiting = lock(&app.awaiting_link);
                match awaiting.take() {
                    Some((id, since)) if since.elapsed() < AWAIT_LINK => Some(id),
                    _ => None,
                }
            };
            if let Some(id) = waiting {
                let manager = handle.state::<App>().manager.clone();
                let headers = capture.request_headers();
                match manager.refresh_link(id, &capture.url, Some(&headers)).await {
                    Ok(()) => {
                        let name = manager
                            .get(id)
                            .await
                            .ok()
                            .and_then(|t| t.name)
                            .unwrap_or_default();
                        to_page(
                            handle,
                            "notice",
                            i18n::t(
                                lang(handle),
                                "Link refreshed: {name} continues",
                                &[("name", &name)],
                            ),
                            false,
                        );
                        return Reply::ok();
                    }
                    Err(e) => {
                        tracing::info!("captured link is not the awaited file: {e}");
                        to_page(
                            handle,
                            "notice",
                            i18n::t(
                                lang(handle),
                                "That link leads to a different file, so it opens as a new download",
                                &[],
                            ),
                            false,
                        );
                    }
                }
            }
            to_page(
                handle,
                "capture",
                serde_json::json!({
                    "url": capture.url,
                    "filename": capture.filename,
                    "size": capture.size,
                    "headers": capture.request_headers(),
                }),
                true,
            );
            Reply::ok()
        }
        Request::Links { referrer, links } => {
            to_page(
                handle,
                "capture-links",
                serde_json::json!({ "referrer": referrer, "links": links }),
                true,
            );
            Reply::ok()
        }
        Request::Media(media) => {
            to_page(
                handle,
                "capture-media",
                serde_json::json!({
                    "page": media.page,
                    "title": media.title,
                    "items": media.items,
                    "headers": media.request_headers(),
                }),
                true,
            );
            Reply::ok()
        }
        Request::Tasks => {
            let app = handle.state::<App>();
            match app.manager.list().await {
                Ok(tasks) => {
                    let live = lock(&app.live).clone();
                    Reply {
                        data: Some(
                            tasks
                                .iter()
                                .map(|t| task_view(t, live.get(&t.id)))
                                .collect(),
                        ),
                        ..Reply::ok()
                    }
                }
                Err(e) => Reply::error(e.to_string()),
            }
        }
        Request::Add(add) => {
            let app = handle.state::<App>();
            let new = NewTask {
                mirrors: clean_mirrors(add.mirrors),
                url: add.url,
                name: add.name.filter(|n| !n.trim().is_empty()),
                dir: add.dir.filter(|d| !d.trim().is_empty()).map(PathBuf::from),
                start: add.start.unwrap_or(true),
                ..NewTask::default()
            };
            match app.manager.add(new).await {
                Ok(t) => Reply {
                    data: Some(task_view(&t, None)),
                    ..Reply::ok()
                },
                Err(e) => Reply::error(e.to_string()),
            }
        }
        Request::Resume { ids } => {
            for_each(handle, ids, |m, id| async move { m.start(id).await }).await
        }
        Request::Pause { ids } => {
            for_each(handle, ids, |m, id| async move { m.pause(id).await }).await
        }
        Request::Remove { ids, delete_files } => {
            for_each(handle, ids, move |m, id| async move {
                m.remove(id, delete_files).await
            })
            .await
        }
        Request::Grab {
            url,
            depth,
            same_site,
        } => {
            let opts = idma_manager::grabber::GrabOptions {
                depth: depth.unwrap_or(1).clamp(1, 5),
                same_site: same_site.unwrap_or(true),
                ..Default::default()
            };
            let manager = handle.state::<App>().manager.clone();
            let cancel = idma_engine::CancellationToken::new();
            match manager.grab_site(&url, &opts, |_| {}, &cancel).await {
                Ok(found) => Reply {
                    data: serde_json::to_value(found).ok(),
                    ..Reply::ok()
                },
                Err(e) => Reply::error(e.to_string()),
            }
        }
        Request::Hello { .. } => Reply::error("unexpected hello"),
    }
}

/// Runs `action` on each download, stopping at the first that fails.
async fn for_each<F, Fut>(handle: &AppHandle, ids: Vec<TaskId>, action: F) -> idma_bridge::Reply
where
    F: Fn(Manager, TaskId) -> Fut,
    Fut: std::future::Future<Output = idma_manager::Result<()>>,
{
    let manager = handle.state::<App>().manager.clone();
    for id in ids {
        if let Err(e) = action(manager.clone(), id).await {
            return idma_bridge::Reply::error(format!("download {id}: {e}"));
        }
    }
    idma_bridge::Reply::ok()
}

/// A download as local tools see it: no request headers, which may hold the browser's
/// cookies.
fn task_view(t: &Task, live: Option<&(u64, f64)>) -> serde_json::Value {
    let (received, speed) = live.copied().unwrap_or((t.received, 0.0));
    serde_json::json!({
        "id": t.id,
        "url": t.url,
        "name": t.name,
        "path": t.path,
        "status": t.status,
        "error": t.error,
        "size": t.size,
        "received": received,
        "speed": speed.round(),
        "added_at": t.added_at,
        "finished_at": t.finished_at,
    })
}

/// Passed when Windows starts the app at sign-in: stay in the tray.
const HIDDEN_ARG: &str = "--hidden";

/// Keeps panics in `crash.log` in the data folder: a release build has no console, and a
/// browser-started app has nowhere else to say why it stopped.
fn log_panics() {
    let Some(dir) = idma_bridge::data_dir() else {
        return;
    };
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;
        let when = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let backtrace = std::backtrace::Backtrace::force_capture();
        if std::fs::create_dir_all(&dir).is_ok()
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("crash.log"))
        {
            let _ = writeln!(
                f,
                "[{when}] {info}
{backtrace}"
            );
        }
        default(info);
    }));
}

/// The app's log: `<data>/logs/idma.log.<date>`, a file a day, the last week kept, and stderr
/// for a console. `DM_LOG` sets the level (default `info`). The guard writes out what is
/// still buffered when the app ends.
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;
    let filter =
        || tracing_subscriber::EnvFilter::try_from_env("DM_LOG").unwrap_or_else(|_| "info".into());
    let console = tracing_subscriber::fmt::layer().with_filter(filter());
    let file = idma_bridge::data_dir().and_then(|data| {
        tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("idma")
            .filename_suffix("log")
            .max_log_files(7)
            .build(data.join(LOG_DIR))
            .ok()
    });
    match file {
        Some(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let file = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter());
            tracing_subscriber::registry()
                .with(console)
                .with(file)
                .init();
            Some(guard)
        }
        None => {
            tracing_subscriber::registry().with(console).init();
            None
        }
    }
}

/// Inside the data folder.
const LOG_DIR: &str = "logs";

/// Opens the folder with the app's logs (and `crash.log` beside it, one level up).
#[tauri::command]
fn open_logs(handle: AppHandle, app: State<'_, App>) -> CmdResult<()> {
    let dir = app.data.join(LOG_DIR);
    std::fs::create_dir_all(&dir).map_err(msg)?;
    handle
        .opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(msg)
}

/// The settings as a file to take to another computer or keep: everything in the settings
/// dialog. Saved logins stay out (their passwords only open on this computer).
#[derive(Serialize, Deserialize)]
struct SettingsFile {
    app: String,
    version: u32,
    settings: Settings,
}

#[tauri::command]
fn export_settings(app: State<'_, App>, path: PathBuf) -> CmdResult<()> {
    let file = SettingsFile {
        app: "idm-alternative".into(),
        version: 1,
        settings: lock(&app.settings).clone(),
    };
    std::fs::write(&path, serde_json::to_vec_pretty(&file).map_err(msg)?).map_err(msg)
}

/// Reads a settings file and applies it; returns the settings now in force. Fields missing
/// from the file keep their defaults.
#[tauri::command]
async fn import_settings(handle: AppHandle, path: PathBuf) -> CmdResult<Settings> {
    let text = std::fs::read_to_string(&path).map_err(msg)?;
    let file: SettingsFile = serde_json::from_str(&text)
        .map_err(|e| format!("this is not an idm-alternative settings file ({e})"))?;
    if file.app != "idm-alternative" {
        return Err("this is not an idm-alternative settings file".into());
    }
    apply_settings(&handle, file.settings).await
}

fn main() {
    log_panics();
    let _log = init_logging();

    let app = tauri::Builder::default()
        // A second launch (e.g. from the browser integration) shows the running window; one
        // with a magnet link or `.torrent` file (opened from Windows) offers to add it.
        .plugin(tauri_plugin_single_instance::init(
            |app, args, cwd| match torrent_arg(&args, Path::new(&cwd)) {
                Some(url) => offer_torrent(app, &url),
                None => show_main(app),
            },
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_ARG]),
        ))
        .setup(|app| {
            // `IDMA_HOME` keeps the list and downloads in one folder of its own, for trying
            // the app out without touching the real list or the Downloads folder.
            let home = std::env::var_os("IDMA_HOME").map(PathBuf::from);
            let data = match &home {
                Some(h) => h.clone(),
                None => app.path().app_data_dir()?,
            };
            std::fs::create_dir_all(&data)?;
            let default_dir = match &home {
                Some(h) => h.join("downloads"),
                None => app
                    .path()
                    .download_dir()
                    .unwrap_or_else(|_| data.join("downloads")),
            };
            let db = data.join("downloads.db");

            let mut settings: Settings =
                tauri::async_runtime::block_on(Manager::read_setting(&db, settings::KEY))?
                    .unwrap_or_default();
            if settings.dir.as_os_str().is_empty() {
                settings.dir = default_dir;
            }
            let ffmpeg = find_on_path("ffmpeg.exe");
            let config = manager_config(&settings, ffmpeg.clone(), &data);
            let manager = tauri::async_runtime::block_on(Manager::open(&db, config))?;
            if settings.browser_integration
                && let Err(e) = set_browser_integration(&data, true)
            {
                tracing::warn!("could not register with the browsers: {e}");
            }
            manager.set_global_limit(settings.speed_limit);

            tauri::async_runtime::spawn(forward_events(app.handle().clone(), manager.subscribe()));

            // The browser bridge: a fresh token per start, a pipe only this user and data
            // folder share.
            let token = idma_bridge::new_token(&data)?;
            let pipe = idma_bridge::pipe_name(&data);
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let serving = idma_bridge::serve(&pipe, token, move |request| {
                    let handle = handle.clone();
                    async move { bridge_reply(&handle, request).await }
                });
                if let Err(e) = serving.await {
                    tracing::error!("browser bridge stopped: {e}");
                }
            });
            let tray = tray::build(
                app.handle(),
                settings.speed_limit,
                i18n::resolve(&settings.language),
            )?;
            app.manage(App {
                manager,
                data: data.clone(),
                settings: Mutex::new(settings),
                tray: Mutex::new(Some(tray)),
                awaiting_link: Mutex::new(None),
                grabbing: Mutex::new(None),
                when_done: Mutex::new(schedule::Armed::default()),
                page: Mutex::new(Page::default()),
                live: Mutex::new(HashMap::new()),
                #[cfg(windows)]
                clipboard: Mutex::new(None),
                ffmpeg,
            });
            tauri::async_runtime::spawn(daily_checks(app.handle().clone()));
            if lock(&app.state::<App>().settings).watch_clipboard {
                watch_clipboard(app.handle(), true);
            }
            tauri::async_runtime::spawn(schedule::run_scheduler(app.handle().clone()));
            tauri::async_runtime::spawn(schedule::run_when_done(app.handle().clone()));
            // The window is not in the config's start-up list (`create: false`): started for
            // the tray, the app opens none.
            let args: Vec<String> = std::env::args().collect();
            let cwd = std::env::current_dir().unwrap_or_default();
            if let Some(url) = torrent_arg(&args, &cwd) {
                offer_torrent(app.handle(), &url);
            } else if !args.iter().any(|a| a == HIDDEN_ARG) {
                show_main(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing to the tray closes the window for real (see `show_main`); the app keeps
            // running because `RunEvent::ExitRequested` is turned down below.
            if let WindowEvent::Destroyed = event
                && window.label() == "main"
            {
                lock(&window.app_handle().state::<App>().page).ready = false;
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_tasks,
            probe,
            add_task,
            start_tasks,
            pause_tasks,
            remove_tasks,
            restart_task,
            start_all,
            pause_all,
            move_task,
            set_speed_limit,
            refresh_link,
            refresh_in_browser,
            inspect_media,
            inspect_torrent,
            grab_site,
            cancel_grab,
            tools,
            install_tool,
            check_update,
            install_update,
            page_ready,
            open_logs,
            export_settings,
            import_settings,
            ui_language,
            show_progress,
            set_when_done,
            get_when_done,
            end_countdown,
            inspect_page,
            set_login,
            batch_preview,
            add_batch,
            saved_logins,
            forget_login,
            open_file,
            show_in_folder,
            get_settings,
            set_settings,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start the app");

    app.run(|app, event| {
        // The last window closing asks to exit (`code` none); with "keep running in the tray"
        // the app stays. Quitting from the tray or "When done" asks with a code and exits.
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = &event
            && lock(&app.state::<App>().settings).close_to_tray
        {
            api.prevent_exit();
        }
        if let RunEvent::Exit = event {
            // Running downloads stop here and resume on the next start.
            let manager = app.state::<App>().manager.clone();
            tauri::async_runtime::block_on(manager.close());
        }
    });
}
