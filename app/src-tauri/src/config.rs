//! The settings: reading and saving them, applying them to the manager and the system,
//! exporting and importing them.

use super::*;

#[tauri::command]
pub(crate) fn get_settings(app: State<'_, App>) -> Settings {
    lock(&app.settings).clone()
}

#[tauri::command]
pub(crate) async fn set_settings(handle: AppHandle, settings: Settings) -> CmdResult<Settings> {
    apply_settings(&handle, settings).await
}

/// Checks, applies and saves new settings, and tells the UI and the tray.
pub(crate) async fn apply_settings(handle: &AppHandle, new: Settings) -> CmdResult<Settings> {
    new.validate()?;
    let app = handle.state::<App>();
    let old = lock(&app.settings).clone();
    app.manager
        .set_config(manager_config(&new, app.path_ffmpeg.clone(), &app.data))
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
            shardload_bridge::register_torrent_links(&exe, shardload_bridge::CLASSES)
        } else {
            shardload_bridge::unregister_torrent_links(&exe, shardload_bridge::CLASSES)
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
        .save_setting(settings::KEY, &new.to_stored())
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
    if new.browser_integration != old.browser_integration {
        let on = new.browser_integration;
        update_extensions(handle, |r| r.integration(on, extensions::now())).await;
    }
    Ok(new)
}

/// Starts or stops listening to the clipboard. A copied download link (one not on the list
/// yet, and not the same as the last one) brings the window up with "Add URL" for it.
pub(crate) fn watch_clipboard(handle: &AppHandle, on: bool) {
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
/// The host is installed next to the app. A profile started with `SHARDLOAD_HOME` registers a
/// small script that starts the host with the same `SHARDLOAD_HOME`, so browsers reach that
/// profile and not the real one. In a Flatpak the host is inside the sandbox, where a browser
/// outside cannot start it: the script, in the app's data folder (which the host sees at the
/// same path), starts it with `flatpak run`.
pub(crate) fn set_browser_integration(data: &Path, on: bool) -> CmdResult<()> {
    if !on {
        return shardload_bridge::unregister(data).map_err(msg);
    }
    let exe = std::env::current_exe().map_err(msg)?;
    let host = exe.with_file_name(if cfg!(windows) {
        "shardload-host.exe"
    } else {
        "shardload-host"
    });
    if !host.exists() {
        return Err(format!(
            "the browser host is missing next to the app ({})",
            host.display()
        ));
    }
    let flatpak = std::env::var("FLATPAK_ID").ok().filter(|id| !id.is_empty());
    let target = if let (Some(id), false) = (&flatpak, cfg!(windows)) {
        let profile = std::env::var_os("SHARDLOAD_HOME").map_or_else(String::new, |_| {
            let dir = data.display();
            format!("--filesystem='{dir}' --env=SHARDLOAD_HOME='{dir}' ")
        });
        let script = data.join("shardload-host.sh");
        let text =
            format!("#!/bin/sh\nexec flatpak run --command=shardload-host {profile}{id} \"$@\"\n");
        write_script(&script, &text)?;
        script
    } else if std::env::var_os("SHARDLOAD_HOME").is_some() {
        let (script, text) = if cfg!(windows) {
            (
                data.join("shardload-host.cmd"),
                format!(
                    "@echo off\nset \"SHARDLOAD_HOME={}\"\nset \"SHARDLOAD_APP={}\"\n\"{}\" %*\n",
                    data.display(),
                    exe.display(),
                    host.display()
                ),
            )
        } else {
            (
                data.join("shardload-host.sh"),
                format!(
                    "#!/bin/sh\nexport SHARDLOAD_HOME='{}'\nexport SHARDLOAD_APP='{}'\nexec '{}' \"$@\"\n",
                    data.display(),
                    exe.display(),
                    host.display()
                ),
            )
        };
        write_script(&script, &text)?;
        script
    } else {
        host
    };
    shardload_bridge::register(&target, data).map_err(msg)
}

/// Writes a script that starts the host, runnable.
fn write_script(script: &Path, text: &str) -> CmdResult<()> {
    std::fs::write(script, text).map_err(msg)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).map_err(msg)?;
    }
    Ok(())
}

/// The manager's configuration: the settings plus the helper programs the app has. ffmpeg:
/// the one on PATH (`path_ffmpeg`), else the one set up, else set up when first needed.
pub(crate) fn manager_config(
    settings: &Settings,
    path_ffmpeg: Option<PathBuf>,
    data: &Path,
) -> Config {
    let ffmpeg = path_ffmpeg.or_else(|| tools::installed(data, tools::Tool::Ffmpeg));
    let setup_data = data.to_owned();
    Config {
        ffmpeg,
        ffmpeg_setup: Some(shardload_manager::FfmpegSetup(std::sync::Arc::new(
            move |opts, progress, cancel| {
                let data = setup_data.clone();
                Box::pin(async move { set_up_ffmpeg(&data, &opts, progress, &cancel).await })
            },
        ))),
        ytdlp: tools::installed(data, tools::Tool::Ytdlp),
        deno: tools::installed(data, tools::Tool::Deno),
        sensor: Some(shardload_manager::net::Sensor(std::sync::Arc::new(
            crate::sensors::read,
        ))),
        ..settings.config()
    }
}

/// Sets up ffmpeg for a download that needs it. One setup at a time: a download that waited
/// for another's uses the program it set up.
pub(crate) async fn set_up_ffmpeg(
    data: &Path,
    opts: &shardload_engine::Options,
    progress: tokio::sync::watch::Sender<shardload_engine::Progress>,
    cancel: &shardload_engine::CancellationToken,
) -> Result<PathBuf, String> {
    static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = tokio::select! {
        held = ONE_AT_A_TIME.lock() => held,
        () = cancel.cancelled() => return Err("cancelled".into()),
    };
    if let Some(exe) = tools::installed(data, tools::Tool::Ffmpeg) {
        return Ok(exe);
    }
    tracing::info!("setting up ffmpeg");
    let exe = tools::install(data, tools::Tool::Ffmpeg, opts, Some(&progress), cancel).await?;
    tracing::info!(
        "set up ffmpeg {}",
        tools::version(&exe).await.unwrap_or_default()
    );
    Ok(exe)
}

/// The settings as a file to take to another computer or keep: everything in the settings
/// dialog. Saved logins and the proxy's password stay out (they would be written in plain
/// text).
#[derive(Serialize, Deserialize)]
pub(crate) struct SettingsFile {
    app: String,
    version: u32,
    settings: Settings,
}

#[tauri::command]
/// Returns whether the proxy's password was left out.
pub(crate) fn export_settings(app: State<'_, App>, path: PathBuf) -> CmdResult<bool> {
    let settings = lock(&app.settings).clone();
    let file = SettingsFile {
        app: "shardload".into(),
        version: 1,
        settings: settings.to_export(),
    };
    std::fs::write(&path, serde_json::to_vec_pretty(&file).map_err(msg)?).map_err(msg)?;
    Ok(settings.proxy_has_password())
}

/// Reads a settings file and applies it; returns the settings now in force. Fields missing
/// from the file keep their defaults.
#[tauri::command]
pub(crate) async fn import_settings(handle: AppHandle, path: PathBuf) -> CmdResult<Settings> {
    let text = std::fs::read_to_string(&path).map_err(msg)?;
    let file: SettingsFile = serde_json::from_str(&text)
        .map_err(|e| format!("this is not a Shardload settings file ({e})"))?;
    // Files saved before the rename carry the old name.
    if !matches!(file.app.as_str(), "shardload" | "idm-alternative") {
        return Err("this is not a Shardload settings file".into());
    }
    apply_settings(&handle, file.settings).await
}
