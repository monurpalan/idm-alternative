//! Desktop app: a thin Tauri shell over `idma-manager`. Commands forward to the manager, and
//! its events go to the web UI as `manager` events (`manager-lagged` when some were dropped
//! and the UI should reload the list).

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
}

/// How long "refresh in browser" waits for the user to start the download again.
const AWAIT_LINK: std::time::Duration = std::time::Duration::from_secs(600);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
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
}

/// HTTP Basic credentials typed by the user.
#[derive(Deserialize)]
struct Login {
    user: String,
    password: String,
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
    match app
        .manager
        .probe(&url, &request_headers(headers, login.as_ref()))
        .await
    {
        Ok(p) => Ok(ProbeView {
            url: p.url,
            name: p.name,
            size: p.size,
            resumable: p.resumable,
            ..ProbeView::default()
        }),
        Err(idma_manager::Error::Engine(idma_engine::Error::AuthRequired { realm })) => {
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
}

#[tauri::command]
async fn add_task(app: State<'_, App>, request: AddRequest) -> CmdResult<Task> {
    app.manager
        .add(NewTask {
            headers: request_headers(request.headers, request.login.as_ref()),
            url: request.url,
            name: request.name.filter(|n| !n.trim().is_empty()),
            dir: request.dir,
            start: request.start,
            stream: request.stream,
            ..NewTask::default()
        })
        .await
        .map_err(msg)
}

/// Credentials for a download whose server asked for them; it continues with them.
#[tauri::command]
async fn set_login(app: State<'_, App>, id: TaskId, login: Login) -> CmdResult<()> {
    app.manager
        .set_login(id, &login.user, &login.password)
        .await
        .map_err(msg)
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
    if let Some(t) = lock(&app.tray).as_ref() {
        t.show_limit(new.speed_limit);
    }
    handle.emit("settings", &new).ok();
    Ok(new)
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
    let host = exe.with_file_name("idma-host.exe");
    if !host.exists() {
        return Err(format!(
            "the browser host is missing next to the app ({})",
            host.display()
        ));
    }
    let target = if std::env::var_os("IDMA_HOME").is_some() {
        let script = data.join("idma-host.cmd");
        std::fs::write(
            &script,
            format!(
                "@echo off
set \"IDMA_HOME={}\"
set \"IDMA_APP={}\"
\"{}\" %*
",
                data.display(),
                exe.display(),
                host.display()
            ),
        )
        .map_err(msg)?;
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

/// Brings the window back, also from the tray.
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        w.show().ok();
        w.unminimize().ok();
        w.set_focus().ok();
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
                            }
                            let before = statuses.insert(t.id, t.status);
                            if t.status == Status::Done && before == Some(Status::Downloading) {
                                notify_done(&handle, t);
                            }
                        }
                        Event::Removed(id) => {
                            pending.remove(id);
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
        .title("Download complete")
        .body(name)
        .show()
    {
        tracing::warn!("could not show a notification: {e}");
    }
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
                        handle
                            .emit("notice", format!("Link refreshed: {name} continues"))
                            .ok();
                        return Reply::ok();
                    }
                    Err(e) => {
                        tracing::info!("captured link is not the awaited file: {e}");
                        handle
                            .emit(
                                "notice",
                                "That link leads to a different file, so it opens as a new download",
                            )
                            .ok();
                    }
                }
            }
            show_main(handle);
            handle
                .emit(
                    "capture",
                    serde_json::json!({
                        "url": capture.url,
                        "filename": capture.filename,
                        "size": capture.size,
                        "headers": capture.request_headers(),
                    }),
                )
                .ok();
            Reply::ok()
        }
        Request::Links { referrer, links } => {
            show_main(handle);
            handle
                .emit(
                    "capture-links",
                    serde_json::json!({ "referrer": referrer, "links": links }),
                )
                .ok();
            Reply::ok()
        }
        Request::Media(media) => {
            show_main(handle);
            handle
                .emit(
                    "capture-media",
                    serde_json::json!({
                        "page": media.page,
                        "title": media.title,
                        "items": media.items,
                        "headers": media.request_headers(),
                    }),
                )
                .ok();
            Reply::ok()
        }
        Request::Hello { .. } => Reply::error("unexpected hello"),
    }
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

fn main() {
    log_panics();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("DM_LOG").unwrap_or_else(|_| "info".into()),
        )
        .init();

    let app = tauri::Builder::default()
        // A second launch (e.g. from the browser integration) shows the running window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
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
            let tray = tray::build(app.handle(), settings.speed_limit)?;
            app.manage(App {
                manager,
                data: data.clone(),
                settings: Mutex::new(settings),
                tray: Mutex::new(Some(tray)),
                awaiting_link: Mutex::new(None),
                ffmpeg,
            });
            if std::env::args().any(|a| a == HIDDEN_ARG)
                && let Some(w) = app.get_webview_window("main")
            {
                w.hide().ok();
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event
                && lock(&window.app_handle().state::<App>().settings).close_to_tray
            {
                api.prevent_close();
                window.hide().ok();
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
            tools,
            install_tool,
            inspect_page,
            set_login,
            open_file,
            show_in_folder,
            get_settings,
            set_settings,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start the app");

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            // Running downloads stop here and resume on the next start.
            let manager = app.state::<App>().manager.clone();
            tauri::async_runtime::block_on(manager.close());
        }
    });
}
