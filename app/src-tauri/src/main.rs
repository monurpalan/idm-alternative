//! Desktop app: a thin Tauri shell over `shardload-manager`. Commands forward to the manager, and
//! its events go to the web UI as `manager` events (`manager-lagged` when some were dropped
//! and the UI should reload the list).

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod add;
mod bridge;
mod bridge_tools;
mod clipboard;
mod config;
mod events;
mod extensions;
mod i18n;
mod logs;
mod scan;
mod schedule;
mod settings;
mod signature;
mod tasks;
mod tools;
mod tray;
mod updates;
mod video;
mod windows;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use settings::Settings;
use shardload_manager::{
    Config, Event, Manager, Move, NewTask, Status, Task, TaskId, TaskProgress,
};
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
    /// ffmpeg found on PATH at start, used instead of setting one up.
    path_ffmpeg: Option<PathBuf>,
    /// A download waiting for the browser to capture a fresh link to it ("refresh in browser"),
    /// and since when.
    awaiting_link: Mutex<Option<(TaskId, std::time::Instant)>>,
    /// Stops the site grab that is running, if any.
    grabbing: Mutex<Option<shardload_engine::CancellationToken>>,
    /// What to do when all downloads finish (one shot).
    when_done: Mutex<schedule::Armed>,
    /// Events waiting for the page, which is closed while the app sits in the tray.
    page: Mutex<Page>,
    /// Latest bytes received, speed and time left of each running download, for local tools
    /// asking over the bridge and the tray (the page gets the same through events).
    live: Mutex<HashMap<TaskId, Live>>,
    /// Listening to the clipboard while the setting is on.
    #[cfg(windows)]
    clipboard: Mutex<Option<clipboard::Watcher>>,
    /// The browser extensions heard from.
    extensions: Mutex<extensions::Record>,
}

/// How long "refresh in browser" waits for the user to start the download again.
const AWAIT_LINK: std::time::Duration = std::time::Duration::from_secs(600);

use add::*;
use bridge::*;
use bridge_tools::*;
use config::*;
use events::*;
use logs::*;
use tasks::*;
use updates::*;
use video::*;
use windows::*;

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

#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "one long sequence kept together; to be split when it next changes"
)]
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
            // `SHARDLOAD_HOME` keeps the list and downloads in one folder of its own, for trying
            // the app out without touching the real list or the Downloads folder.
            let home = std::env::var_os("SHARDLOAD_HOME").map(PathBuf::from);
            let data = match &home {
                Some(h) => h.clone(),
                None => app.path().app_data_dir()?,
            };
            if home.is_none() {
                shardload_bridge::migrate_legacy_data(&data);
            }
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
                    .map(Settings::restored)
                    .unwrap_or_default();
            if settings.dir.as_os_str().is_empty() {
                settings.dir = default_dir;
            }
            let mut seen: extensions::Record =
                tauri::async_runtime::block_on(Manager::read_setting(&db, extensions::KEY))?
                    .unwrap_or_default();
            seen.integration(settings.browser_integration, extensions::now());
            // One the user installed is kept: they may want their own build.
            let path_ffmpeg = find_on_path("ffmpeg");
            let config = manager_config(&settings, path_ffmpeg.clone(), &data);
            let manager = tauri::async_runtime::block_on(Manager::open(&db, config))?;
            if settings.browser_integration
                && let Err(e) = set_browser_integration(&data, true)
            {
                tracing::warn!("could not register with the browsers: {e}");
            }
            if let Err(e) =
                tauri::async_runtime::block_on(manager.save_setting(extensions::KEY, &seen))
            {
                tracing::warn!("could not save the extensions heard from: {e}");
            }
            manager.set_global_limit(settings.speed_limit);

            tauri::async_runtime::spawn(forward_events(app.handle().clone(), manager.subscribe()));

            // The browser bridge: a fresh token per start, a pipe only this user and data
            // folder share.
            let token = shardload_bridge::new_token(&data)?;
            let pipe = shardload_bridge::pipe_name(&data);
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let serving = shardload_bridge::serve(&pipe, token, move |request| {
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
                extensions: Mutex::new(seen),
                path_ffmpeg,
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
            #[cfg(windows)]
            if let WindowEvent::Resized(_) = event
                && let Some(w) = window.app_handle().get_webview_window(window.label())
            {
                memory_target(&w, w.is_minimized().unwrap_or(false));
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_tasks,
            disk_needs,
            parse_command,
            parse_headers,
            set_headers,
            probe,
            add_task,
            start_tasks,
            pause_tasks,
            remove_tasks,
            restart_task,
            rename_task,
            relocate_task,
            start_all,
            pause_all,
            move_task,
            set_speed_limit,
            refresh_link,
            refresh_in_browser,
            inspect_media,
            inspect_torrent,
            inspect_archive,
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
            add_playlist,
            list_groups,
            new_group,
            find_previous,
            reuse_previous,
            known_links,
            open_previous,
            show_previous,
            clear_history,
            sites,
            site_memory,
            link_warnings,
            forget_site,
            file_origin,
            dropped_file_origin,
            check_file_checksum,
            check_for_update,
            saved_logins,
            forget_login,
            open_file,
            show_in_folder,
            get_settings,
            set_settings,
            extension_status,
            dismiss_extension_warning,
            install_extension,
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

#[cfg(test)]
mod tests {
    use super::*;

    const YOUTUBE: &str = "https://www.youtube.com/watch?v=9bZkp7q19f0";

    #[test]
    fn dates() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400_000), "2000-02-29");
        assert_eq!(utc_date(1_790_985_600_000), "2026-10-03");
        assert_eq!(utc_date(-86_400_000), "1969-12-31");
    }

    /// A page through the manager and yt-dlp, the way the app adds it, at up to `height`, with
    /// ffmpeg missing. `SHARDLOAD_TOOLS` is a tools folder with yt-dlp and deno set up. Run with no
    /// ffmpeg on PATH, so that yt-dlp cannot find one by itself.
    async fn page(
        url: &str,
        height: u32,
        setup: Option<shardload_manager::FfmpegSetup>,
    ) -> (Task, bool, PathBuf) {
        let tools_from = PathBuf::from(std::env::var("SHARDLOAD_TOOLS").expect("SHARDLOAD_TOOLS"));
        let data = std::env::temp_dir().join(format!(
            "shardload-youtube-{}-{}",
            std::process::id(),
            setup.is_some()
        ));
        std::fs::remove_dir_all(&data).ok();
        std::fs::create_dir_all(tools::dir(&data)).expect("tools folder");
        for exe in ["yt-dlp.exe", "deno.exe"] {
            std::fs::copy(tools_from.join(exe), tools::dir(&data).join(exe)).expect("copy");
        }
        let settings = Settings {
            dir: data.join("downloads"),
            ..Settings::default()
        };
        let mut config = manager_config(&settings, None, &data);
        assert!(config.ffmpeg.is_none(), "no ffmpeg set up yet");
        if let Some(setup) = setup {
            config.ffmpeg_setup = Some(setup);
        }
        let manager = Manager::open(&data.join("downloads.db"), config)
            .await
            .expect("open");
        let mut events = manager.subscribe();
        let added = manager
            .add(NewTask {
                url: url.into(),
                name: Some("clip.mp4".into()),
                start: true,
                stream: Some(shardload_manager::Stream {
                    ytdlp: Some(shardload_manager::ytdlp::selector(Some(height))),
                    ..Default::default()
                }),
                ..NewTask::default()
            })
            .await
            .expect("add");
        let mut set_up = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(900);
        let task = loop {
            let event = tokio::time::timeout_at(deadline, events.recv())
                .await
                .expect("in time")
                .expect("event");
            match event {
                Event::Progress(p) if p.id == added.id => {
                    set_up |= p.phase == shardload_engine::Phase::SettingUp;
                }
                Event::Updated(t)
                    if t.id == added.id && matches!(t.status, Status::Done | Status::Error) =>
                {
                    break manager.get(t.id).await.expect("task");
                }
                _ => {}
            }
        };
        manager.close().await;
        (task, set_up, data)
    }

    /// The streams in a file, by ffprobe: `video,1080` and `audio,` lines.
    fn streams(ffprobe: &Path, file: &Path) -> String {
        let out = std::process::Command::new(ffprobe)
            .args(["-v", "error", "-show_entries", "stream=codec_type,height"])
            .args(["-of", "csv=p=0"])
            .arg(file)
            .output()
            .expect("ffprobe");
        String::from_utf8_lossy(&out.stdout).replace('\r', "")
    }

    fn files_in(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .expect("folder")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "downloads ffmpeg and a YouTube video"]
    async fn youtube_1080p_sets_up_ffmpeg_and_joins() {
        let (task, set_up, data) = page(YOUTUBE, 1080, None).await;
        assert_eq!(task.status, Status::Done, "{:?}", task.error);
        assert!(set_up, "the setup's progress was shown");
        assert_eq!(task.error, None);
        let ffmpeg = tools::installed(&data, tools::Tool::Ffmpeg).expect("set up");
        let file = task.path.expect("path");
        assert_eq!(files_in(&data.join("downloads")), ["clip.mp4"]);
        let found = streams(&ffmpeg.with_file_name("ffprobe.exe"), &file);
        assert!(found.contains("video,1080"), "{found}");
        assert!(found.contains("audio"), "{found}");
        std::fs::remove_dir_all(&data).ok();
    }

    fn offline() -> shardload_manager::FfmpegSetup {
        shardload_manager::FfmpegSetup(std::sync::Arc::new(|_, _, _| {
            Box::pin(async { Err("offline".to_owned()) })
        }))
    }

    /// Without ffmpeg, a site that offers formats with both video and sound gives one of them
    /// (here the Internet Archive's 360p MP4), and the download says why.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "downloads a video from the Internet Archive"]
    async fn without_ffmpeg_a_page_is_one_file_with_sound() {
        let url = "https://archive.org/details/BigBuckBunny_124";
        let (task, _, data) = page(url, 480, Some(offline())).await;
        assert_eq!(task.status, Status::Done, "{:?}", task.error);
        let note = task.error.expect("the download says why");
        assert!(
            note.contains("lower quality") && note.contains("offline"),
            "{note}"
        );
        let names = files_in(&data.join("downloads"));
        assert_eq!(names.len(), 1, "{names:?}");
        let ffprobe = PathBuf::from(std::env::var("SHARDLOAD_FFPROBE").expect("SHARDLOAD_FFPROBE"));
        let found = streams(&ffprobe, &task.path.expect("path"));
        assert!(
            found.contains("video") && found.contains("audio"),
            "{found}"
        );
        std::fs::remove_dir_all(&data).ok();
    }

    /// Without ffmpeg, YouTube (which offers no format with both any more) fails and says why,
    /// instead of leaving a silent video next to its sound.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "asks YouTube for a video"]
    async fn without_ffmpeg_youtube_says_why_and_leaves_no_files() {
        let (task, _, data) = page(YOUTUBE, 1080, Some(offline())).await;
        assert_eq!(task.status, Status::Error);
        let error = task.error.expect("why");
        assert!(
            error.contains("separate video and audio") && error.contains("offline"),
            "{error}"
        );
        let dir = data.join("downloads");
        let names = if dir.exists() {
            files_in(&dir)
        } else {
            Vec::new()
        };
        assert!(names.is_empty(), "{names:?}");
        std::fs::remove_dir_all(&data).ok();
    }
}
