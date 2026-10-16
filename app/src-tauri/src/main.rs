//! Desktop app: a thin Tauri shell over `idma-manager`. Commands forward to the manager, and
//! its events go to the web UI as `manager` events (`manager-lagged` when some were dropped
//! and the UI should reload the list).

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::path::PathBuf;

use idma_manager::{Config, Event, Manager, Move, NewTask, Status, Task, TaskId, TaskProgress};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager as _, RunEvent, State};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::broadcast::{self, error::RecvError};

struct App {
    manager: Manager,
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
#[derive(Serialize)]
struct ProbeView {
    url: String,
    name: String,
    size: Option<u64>,
    resumable: bool,
}

#[tauri::command]
async fn probe(app: State<'_, App>, url: String) -> CmdResult<ProbeView> {
    let p = app.manager.probe(&url, &[]).await.map_err(msg)?;
    Ok(ProbeView {
        url: p.url,
        name: p.name,
        size: p.size,
        resumable: p.resumable,
    })
}

#[derive(Deserialize)]
struct AddRequest {
    url: String,
    name: Option<String>,
    dir: Option<PathBuf>,
    start: bool,
}

#[tauri::command]
async fn add_task(app: State<'_, App>, request: AddRequest) -> CmdResult<Task> {
    app.manager
        .add(NewTask {
            url: request.url,
            name: request.name.filter(|n| !n.trim().is_empty()),
            dir: request.dir,
            start: request.start,
            ..NewTask::default()
        })
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

/// Continues a download from a fresh link to the same file.
#[tauri::command]
async fn refresh_link(app: State<'_, App>, id: TaskId, url: String) -> CmdResult<()> {
    app.manager
        .refresh_link(id, url.trim())
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

#[derive(Serialize)]
struct Settings {
    dir: PathBuf,
    max_active: usize,
}

#[tauri::command]
fn get_settings(app: State<'_, App>) -> Settings {
    let config = app.manager.config();
    Settings {
        dir: config.dir,
        max_active: config.max_active,
    }
}

/// Downloads running at once; more start right away when raised.
#[tauri::command]
async fn set_max_active(app: State<'_, App>, max_active: usize) -> CmdResult<()> {
    let mut config = app.manager.config();
    config.max_active = max_active.clamp(1, 50);
    app.manager.set_config(config).await.map_err(msg)
}

/// How often the UI gets the progress of all running downloads, as one message.
const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// Sends manager events to the UI. List changes go out at once; progress is collected and
/// sent for all downloads together a few times a second, so the UI handles one message per
/// tick instead of one per download (with hundreds of rows, one each could outpace it).
async fn forward_events(handle: tauri::AppHandle, mut events: broadcast::Receiver<Event>) {
    let mut pending: HashMap<TaskId, TaskProgress> = HashMap::new();
    let mut tick = tokio::time::interval(PROGRESS_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            e = events.recv() => match e {
                Ok(Event::Progress(p)) => {
                    pending.insert(p.id, p);
                }
                Ok(e) => {
                    // A download that stopped must not get a stale progress afterwards.
                    if let Event::Updated(t) = &e
                        && t.status != Status::Downloading
                    {
                        pending.remove(&t.id);
                    }
                    if let Event::Removed(id) = &e {
                        pending.remove(id);
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

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("DM_LOG").unwrap_or_else(|_| "info".into()),
        )
        .init();

    let app = tauri::Builder::default()
        // A second launch (e.g. from the browser integration) focuses the running window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                w.unminimize().ok();
                w.set_focus().ok();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // `IDMA_HOME` keeps the list and downloads in one folder of its own, for trying
            // the app out without touching the real list or the Downloads folder.
            let home = std::env::var_os("IDMA_HOME").map(PathBuf::from);
            let data = match &home {
                Some(h) => h.clone(),
                None => app.path().app_data_dir()?,
            };
            std::fs::create_dir_all(&data)?;
            let dir = match &home {
                Some(h) => h.join("downloads"),
                None => app
                    .path()
                    .download_dir()
                    .unwrap_or_else(|_| data.join("downloads")),
            };
            let config = Config {
                dir,
                ..Config::default()
            };
            let manager =
                tauri::async_runtime::block_on(Manager::open(&data.join("downloads.db"), config))?;

            tauri::async_runtime::spawn(forward_events(app.handle().clone(), manager.subscribe()));
            app.manage(App { manager });
            Ok(())
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
            open_file,
            show_in_folder,
            get_settings,
            set_max_active,
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
