//! What is done to downloads on the list: start, pause, remove, rename and move,
//! headers and logins, a fresh link, torrents and archives, opening the file.

use super::*;

/// A `curl`, `wget` or `Invoke-WebRequest` command pasted in the "New download" window.
#[tauri::command]
pub(crate) fn parse_command(text: String) -> CmdResult<shardload_manager::command::Pasted> {
    shardload_manager::command::parse_command(&text)
}

/// `Name: value` lines typed by the user, checked.
#[tauri::command]
pub(crate) fn parse_headers(text: String) -> CmdResult<Vec<(String, String)>> {
    shardload_manager::command::parse_header_lines(&text)
}

/// The request headers a download is sent with from now on, as `Name: value` lines.
#[tauri::command]
pub(crate) async fn set_headers(app: State<'_, App>, id: TaskId, text: String) -> CmdResult<Task> {
    let headers = shardload_manager::command::parse_header_lines(&text)?;
    app.manager.set_headers(id, &headers).await.map_err(msg)
}

/// Credentials for a download whose server asked for them; it continues with them.
#[tauri::command]
pub(crate) async fn set_login(app: State<'_, App>, id: TaskId, login: Login) -> CmdResult<()> {
    let task = app.manager.get(id).await.map_err(msg)?;
    remember_login(&app, &task.url, Some(&login)).await?;
    app.manager
        .set_login(id, &login.user, &login.password)
        .await
        .map_err(msg)
}

/// Sites with a remembered login, for the settings.
#[tauri::command]
pub(crate) async fn saved_logins(
    app: State<'_, App>,
) -> CmdResult<Vec<shardload_manager::SavedLogin>> {
    app.manager.saved_logins().await.map_err(msg)
}

#[tauri::command]
pub(crate) async fn forget_login(app: State<'_, App>, site: String) -> CmdResult<()> {
    app.manager.forget_login(&site).await.map_err(msg)
}

#[tauri::command]
pub(crate) async fn start_tasks(app: State<'_, App>, ids: Vec<TaskId>) -> CmdResult<()> {
    for id in ids {
        app.manager.start(id).await.map_err(msg)?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn pause_tasks(app: State<'_, App>, ids: Vec<TaskId>) -> CmdResult<()> {
    for id in ids {
        app.manager.pause(id).await.map_err(msg)?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn remove_tasks(
    app: State<'_, App>,
    ids: Vec<TaskId>,
    delete_files: bool,
) -> CmdResult<()> {
    for id in ids {
        app.manager.remove(id, delete_files).await.map_err(msg)?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn start_all(app: State<'_, App>) -> CmdResult<()> {
    app.manager.start_all().await.map_err(msg)
}

#[tauri::command]
pub(crate) async fn pause_all(app: State<'_, App>) -> CmdResult<()> {
    app.manager.pause_all().await.map_err(msg)
}

#[tauri::command]
pub(crate) async fn move_task(app: State<'_, App>, id: TaskId, to: Move) -> CmdResult<()> {
    app.manager.move_task(id, to).await.map_err(msg)
}

#[tauri::command]
pub(crate) async fn restart_task(app: State<'_, App>, id: TaskId) -> CmdResult<()> {
    app.manager.restart(id).await.map_err(msg)
}

/// A new file name; the download's files on disk follow.
#[tauri::command]
pub(crate) async fn rename_task(app: State<'_, App>, id: TaskId, name: String) -> CmdResult<Task> {
    if name.trim().is_empty() {
        return Err("the name is empty".into());
    }
    app.manager.rename(id, &name).await.map_err(msg)
}

/// Another folder, chosen by the user; the download's files on disk follow.
#[tauri::command]
pub(crate) async fn relocate_task(
    app: State<'_, App>,
    id: TaskId,
    dir: PathBuf,
) -> CmdResult<Task> {
    app.manager.relocate(id, &dir).await.map_err(msg)
}

/// This download's own limit in bytes per second; `None` removes it.
#[tauri::command]
pub(crate) async fn set_speed_limit(
    app: State<'_, App>,
    id: TaskId,
    rate: Option<u64>,
) -> CmdResult<()> {
    app.manager
        .set_speed_limit(id, rate.filter(|r| *r > 0))
        .await
        .map_err(msg)
}

/// Opens the page the download came from (or its address) in the default browser and waits
/// for the extension to capture the download again: the captured link then continues this
/// download instead of starting a new one.
#[tauri::command]
pub(crate) async fn refresh_in_browser(
    handle: AppHandle,
    app: State<'_, App>,
    id: TaskId,
) -> CmdResult<()> {
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
pub(crate) fn torrent_arg(args: &[String], cwd: &Path) -> Option<String> {
    args.iter().skip(1).find_map(|a| {
        if a.to_ascii_lowercase().starts_with("magnet:") {
            return Some(a.clone());
        }
        let path = cwd.join(a);
        (shardload_manager::torrent::is_torrent(a) && path.is_file())
            .then(|| path.to_string_lossy().into_owned())
    })
}

/// Opens "Add download" with the torrent, as for a download from the browser.
pub(crate) fn offer_torrent(handle: &AppHandle, url: &str) {
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
pub(crate) async fn grab_site(
    handle: AppHandle,
    app: State<'_, App>,
    url: String,
    depth: u32,
    same_site: bool,
) -> CmdResult<Vec<shardload_manager::grabber::Found>> {
    let cancel = shardload_engine::CancellationToken::new();
    if let Some(old) = lock(&app.grabbing).replace(cancel.clone()) {
        old.cancel();
    }
    let opts = shardload_manager::grabber::GrabOptions {
        depth: depth.clamp(1, 5),
        same_site,
        ..Default::default()
    };
    let progress = |p: shardload_manager::grabber::GrabProgress| {
        handle.emit("grab-progress", p).ok();
    };
    let result = app.manager.grab_site(&url, &opts, progress, &cancel).await;
    lock(&app.grabbing).take();
    result.map_err(msg)
}

#[tauri::command]
pub(crate) fn cancel_grab(app: State<'_, App>) {
    if let Some(c) = lock(&app.grabbing).take() {
        c.cancel();
    }
}

/// A torrent's name and files (a magnet link asks peers for them), for picking files.
#[tauri::command]
pub(crate) async fn inspect_torrent(
    app: State<'_, App>,
    url: String,
) -> CmdResult<shardload_manager::TorrentInfo> {
    app.manager.inspect_torrent(&url).await.map_err(msg)
}

/// The files in a ZIP archive on a server that supports range requests, for picking some.
#[tauri::command]
pub(crate) async fn inspect_archive(
    app: State<'_, App>,
    url: String,
    login: Option<Login>,
    headers: Option<Vec<(String, String)>>,
) -> CmdResult<shardload_manager::ArchiveInfo> {
    // The site's saved login, which the download would use too.
    let saved = match &login {
        Some(_) => None,
        None => app
            .manager
            .saved_login(&url)
            .await
            .ok()
            .flatten()
            .map(|(user, password)| Login {
                user,
                password,
                remember: false,
            }),
    };
    let headers = request_headers(headers, login.as_ref().or(saved.as_ref()));
    app.manager
        .inspect_archive(&url, &headers)
        .await
        .map_err(msg)
}

/// What an HLS or DASH stream offers (qualities, audio, subtitles), read with the page's
/// headers.
#[tauri::command]
pub(crate) async fn inspect_media(
    app: State<'_, App>,
    url: String,
    headers: Option<Vec<(String, String)>>,
) -> CmdResult<shardload_media::Streams> {
    let opts = app.manager.config().engine;
    let headers = headers.unwrap_or_default();
    let (text, final_url) = shardload_engine::fetch_text(&url, &headers, &opts, 8 << 20)
        .await
        .map_err(msg)?;
    let mut streams = shardload_media::parse(&final_url, &text).map_err(msg)?;
    // A master playlist does not say whether it is live or how long: its first variant does.
    if streams.format == shardload_media::Format::Hls
        && streams.duration.is_none()
        && !streams.live
        && let Some(variant) = streams.video.first()
        && let Ok((text, variant_url)) =
            shardload_engine::fetch_text(&variant.id, &headers, &opts, 8 << 20).await
        && let Ok(media) = shardload_media::parse(&variant_url, &text)
    {
        streams.live = media.live;
        streams.duration = media.duration;
    }
    Ok(streams)
}

/// Continues a download from a fresh link to the same file.
#[tauri::command]
pub(crate) async fn refresh_link(app: State<'_, App>, id: TaskId, url: String) -> CmdResult<()> {
    app.manager
        .refresh_link(id, url.trim(), None)
        .await
        .map_err(|e| match e {
            shardload_manager::Error::Engine(shardload_engine::Error::FileChanged) => {
                "this link leads to a different file (size or content differ)".to_owned()
            }
            e => e.to_string(),
        })
}

/// The file of a download, looked up here so the UI never names arbitrary paths to open.
pub(crate) async fn file_of(app: &App, id: TaskId) -> CmdResult<PathBuf> {
    let task = app.manager.get(id).await.map_err(msg)?;
    task.path
        .ok_or_else(|| "this download has no file yet".to_owned())
}

#[tauri::command]
pub(crate) async fn open_file(
    handle: tauri::AppHandle,
    app: State<'_, App>,
    id: TaskId,
) -> CmdResult<()> {
    let path = file_of(&app, id).await?;
    handle
        .opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(msg)
}

#[tauri::command]
pub(crate) async fn show_in_folder(
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
        part.push(".shardload-part");
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
