//! Desktop app: a thin Tauri shell over `idma-manager`. Commands forward to the manager, and
//! its events go to the web UI as `manager` events (`manager-lagged` when some were dropped
//! and the UI should reload the list).

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use idma_manager::{Config, Manager, NewTask, Task, TaskId};
use tauri::{Emitter, Manager as _, RunEvent, State};
use tokio::sync::broadcast::error::RecvError;

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

#[tauri::command]
async fn add_task(app: State<'_, App>, url: String) -> CmdResult<Task> {
    app.manager
        .add(NewTask {
            url,
            start: true,
            ..NewTask::default()
        })
        .await
        .map_err(msg)
}

#[tauri::command]
async fn start_task(app: State<'_, App>, id: TaskId) -> CmdResult<()> {
    app.manager.start(id).await.map_err(msg)
}

#[tauri::command]
async fn pause_task(app: State<'_, App>, id: TaskId) -> CmdResult<()> {
    app.manager.pause(id).await.map_err(msg)
}

#[tauri::command]
async fn remove_task(app: State<'_, App>, id: TaskId, delete_files: bool) -> CmdResult<()> {
    app.manager.remove(id, delete_files).await.map_err(msg)
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
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            let dir = app
                .path()
                .download_dir()
                .unwrap_or_else(|_| data.join("downloads"));
            let config = Config {
                dir,
                ..Config::default()
            };
            let manager =
                tauri::async_runtime::block_on(Manager::open(&data.join("downloads.db"), config))?;

            let mut events = manager.subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(e) => {
                            handle.emit("manager", &e).ok();
                        }
                        Err(RecvError::Lagged(_)) => {
                            handle.emit("manager-lagged", ()).ok();
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
            });
            app.manage(App { manager });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_tasks,
            add_task,
            start_task,
            pause_task,
            remove_task
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
