//! Adding downloads: asking the server first, batches, the history of earlier
//! downloads and where a file came from, the sites' memory.

use super::*;

#[tauri::command]
pub(crate) async fn list_tasks(app: State<'_, App>) -> CmdResult<Vec<Task>> {
    app.manager.list().await.map_err(msg)
}

/// What the "Add URL" dialog shows before the user decides.
#[derive(Serialize, Default)]
pub(crate) struct ProbeView {
    url: String,
    name: String,
    size: Option<u64>,
    resumable: bool,
    /// The server asks for a user name and password first.
    needs_login: bool,
    realm: Option<String>,
    /// SFTP: the user's SSH key that needs its passphrase (given as the password), and
    /// whether the password given was not it.
    ssh_key: Option<String>,
    wrong_passphrase: bool,
    /// The login saved for the site answered it: its user name.
    saved_login: Option<String>,
}

/// HTTP Basic credentials typed by the user.
#[derive(Deserialize)]
pub(crate) struct Login {
    pub(crate) user: String,
    pub(crate) password: String,
    /// Save it for the site, so its downloads log in by themselves.
    #[serde(default)]
    pub(crate) remember: bool,
}

/// Saves `login` for the site of `url` when the user asked to.
pub(crate) async fn remember_login(app: &App, url: &str, login: Option<&Login>) -> CmdResult<()> {
    let Some(login) = login.filter(|l| l.remember) else {
        return Ok(());
    };
    let site = shardload_manager::site_of(url).ok_or("this address has no site to remember")?;
    app.manager
        .save_login(&site, &login.user, &login.password)
        .await
        .map_err(msg)
}

pub(crate) fn login_headers(login: Option<&Login>) -> Vec<(String, String)> {
    login
        .map(|l| shardload_engine::basic_auth(&l.user, &l.password))
        .into_iter()
        .collect()
}

/// Headers the request carries: those the browser handed over, plus any login.
pub(crate) fn request_headers(
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
pub(crate) async fn probe(
    app: State<'_, App>,
    url: String,
    login: Option<Login>,
    headers: Option<Vec<(String, String)>>,
) -> CmdResult<ProbeView> {
    let view = |p: shardload_engine::Probe, saved_login| ProbeView {
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
        Err(shardload_manager::Error::Engine(
            e @ (shardload_engine::Error::AuthRequired { .. }
            | shardload_engine::Error::KeyLocked { .. }),
        )) => {
            let (realm, ssh_key, wrong_passphrase) = match e {
                shardload_engine::Error::KeyLocked { key, wrong } => (None, Some(key), wrong),
                shardload_engine::Error::AuthRequired { realm } => (realm, None, false),
                _ => (None, None, false),
            };
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
                ssh_key,
                wrong_passphrase,
                ..ProbeView::default()
            })
        }
        Err(e) => Err(msg(e)),
    }
}

#[derive(Deserialize)]
pub(crate) struct AddRequest {
    url: String,
    name: Option<String>,
    dir: Option<PathBuf>,
    start: bool,
    login: Option<Login>,
    /// From the browser: cookies, referrer, user agent.
    headers: Option<Vec<(String, String)>>,
    /// A stream: `url` is then the chosen video playlist.
    stream: Option<shardload_manager::Stream>,
    /// Other addresses of the same file.
    #[serde(default)]
    mirrors: Vec<String>,
    /// A batch: leave out the files downloaded before or on the list.
    #[serde(default)]
    skip_known: bool,
    /// The title of the page the link came from.
    #[serde(default)]
    title: Option<String>,
    /// A checksum shown next to the link on that page.
    #[serde(default)]
    page_checksum: Option<String>,
    /// The size the server said when asked.
    #[serde(default)]
    size: Option<u64>,
    /// One of a group added together.
    #[serde(default)]
    group: Option<i64>,
    /// Unpack or move it once finished.
    #[serde(default)]
    after: Option<shardload_manager::after::After>,
}

/// An earlier download of what is about to be added: `final_url`, `name` and `size` are
/// what the server said when asked; `checksum` the one it is expected to have (shown on its
/// page), for finding the same content under another address; `dir` where it would go.
#[tauri::command]
#[allow(clippy::too_many_arguments, reason = "a command's named arguments")]
pub(crate) async fn find_previous(
    app: State<'_, App>,
    url: String,
    final_url: Option<String>,
    name: Option<String>,
    size: Option<u64>,
    checksum: Option<String>,
    headers: Option<Vec<(String, String)>>,
    dir: Option<PathBuf>,
) -> CmdResult<Option<shardload_manager::Previous>> {
    let found = app
        .manager
        .find_previous(&url, final_url.as_deref(), name.as_deref(), size)
        .await
        .map_err(msg)?;
    if found.is_some() {
        return Ok(found);
    }
    app.manager
        .find_same_content(
            final_url.as_deref().unwrap_or(&url),
            name.as_deref(),
            size,
            checksum.as_deref(),
            &headers.unwrap_or_default(),
            dir.as_deref().filter(|d| !d.as_os_str().is_empty()),
        )
        .await
        .map_err(msg)
}

/// Puts the file of an earlier download with the same content into place, copied or as a
/// hard link, instead of downloading it again.
#[tauri::command]
pub(crate) async fn reuse_previous(
    app: State<'_, App>,
    history: i64,
    dir: Option<PathBuf>,
    name: Option<String>,
    how: shardload_manager::Reuse,
) -> CmdResult<Task> {
    app.manager
        .reuse(
            history,
            dir.filter(|d| !d.as_os_str().is_empty()),
            name,
            how,
        )
        .await
        .map_err(msg)
}

/// For each link, whether it was downloaded before or is on the list (by address only).
#[tauri::command]
pub(crate) async fn known_links(app: State<'_, App>, urls: Vec<String>) -> CmdResult<Vec<bool>> {
    let mut known = Vec::with_capacity(urls.len());
    for url in &urls {
        known.push(
            app.manager
                .find_previous(url, None, None, None)
                .await
                .map_err(msg)?
                .is_some(),
        );
    }
    Ok(known)
}

/// The file of a finished download in the history, looked up here so the UI never names
/// arbitrary paths to open.
pub(crate) async fn history_file(app: &App, id: i64) -> CmdResult<PathBuf> {
    let entry = app.manager.history_entry(id).await.map_err(msg)?;
    let path = entry
        .ok_or_else(|| "this download is no longer in the history".to_owned())?
        .path;
    if !path.exists() {
        return Err(format!("{} is no longer there", path.display()));
    }
    Ok(path)
}

#[tauri::command]
pub(crate) async fn open_previous(
    handle: AppHandle,
    app: State<'_, App>,
    history: i64,
) -> CmdResult<()> {
    let path = history_file(&app, history).await?;
    handle
        .opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(msg)
}

#[tauri::command]
pub(crate) async fn show_previous(
    handle: AppHandle,
    app: State<'_, App>,
    history: i64,
) -> CmdResult<()> {
    let path = history_file(&app, history).await?;
    handle.opener().reveal_item_in_dir(path).map_err(msg)
}

/// Where the file at `path` came from (a file the user chose).
#[tauri::command]
pub(crate) async fn file_origin(
    app: State<'_, App>,
    path: PathBuf,
) -> CmdResult<shardload_manager::origin::Origin> {
    app.manager.file_origin(&path).await.map_err(msg)
}

/// Where a dropped file came from, known by its size and content (or name): the page gets
/// no path for it.
#[tauri::command]
pub(crate) async fn dropped_file_origin(
    app: State<'_, App>,
    size: u64,
    sha256: Option<String>,
    name: Option<String>,
) -> CmdResult<Option<shardload_manager::origin::Origin>> {
    let found = app
        .manager
        .find_by_content(size, sha256.as_deref(), name.as_deref())
        .await
        .map_err(msg)?;
    Ok(found.map(|h| shardload_manager::origin::Origin {
        path: h.path.clone(),
        size,
        traced: shardload_manager::origin::Traced::Content,
        url: Some(h.final_url.clone().unwrap_or_else(|| h.url.clone())),
        page: h.page.clone(),
        downloaded_at: Some(h.finished_at),
        checksum: h.checksum.clone(),
        history: Some(h),
    }))
}

/// The file hashed again against its download's checksum, or the one its publisher posts.
#[tauri::command]
pub(crate) async fn check_file_checksum(
    app: State<'_, App>,
    path: PathBuf,
    checksum: Option<String>,
    url: Option<String>,
) -> CmdResult<shardload_manager::origin::ChecksumCheck> {
    app.manager
        .check_file_checksum(&path, checksum.as_deref(), url.as_deref())
        .await
        .map_err(msg)
}

/// Whether the file's address serves something newer now.
#[tauri::command]
pub(crate) async fn check_for_update(
    app: State<'_, App>,
    url: String,
    size: u64,
    downloaded_at: Option<i64>,
) -> CmdResult<shardload_manager::origin::UpdateCheck> {
    app.manager
        .check_for_update(&url, size, downloaded_at)
        .await
        .map_err(msg)
}

/// What downloads learned about each site, for the settings.
#[tauri::command]
pub(crate) async fn sites(app: State<'_, App>) -> CmdResult<Vec<shardload_manager::SiteMemory>> {
    app.manager.sites().await.map_err(msg)
}

/// What is remembered about the site of a download's address, for its details.
#[tauri::command]
pub(crate) async fn site_memory(
    app: State<'_, App>,
    url: String,
) -> CmdResult<Option<shardload_manager::SiteMemory>> {
    app.manager.site_memory(&url).await.map_err(msg)
}

/// Waiting downloads whose links, by what was learned of their sites, are about to expire.
#[tauri::command]
pub(crate) async fn link_warnings(
    app: State<'_, App>,
) -> CmdResult<Vec<shardload_manager::LinkWarning>> {
    app.manager.link_warnings().await.map_err(msg)
}

#[tauri::command]
pub(crate) async fn forget_site(app: State<'_, App>, site: String) -> CmdResult<()> {
    app.manager.forget_site(&site).await.map_err(msg)
}

/// Forgets the finished downloads (for "you downloaded this before"); files stay.
#[tauri::command]
pub(crate) async fn clear_history(app: State<'_, App>) -> CmdResult<()> {
    app.manager.clear_history().await.map_err(msg)
}

#[tauri::command]
pub(crate) async fn add_task(app: State<'_, App>, request: AddRequest) -> CmdResult<Task> {
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
            group: request.group,
            title: request.title,
            page_checksum: request.page_checksum,
            size: request.size,
            after: request.after,
            ..NewTask::default()
        })
        .await
        .map_err(msg)
}

/// Mirrors as typed or pasted: trimmed, empty lines and repeats left out.
pub(crate) fn clean_mirrors(mirrors: Vec<String>) -> Vec<String> {
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
pub(crate) struct BatchView {
    count: usize,
    first: String,
    last: String,
    /// How many of them were downloaded before or are on the list.
    known: usize,
}

#[tauri::command]
pub(crate) async fn batch_preview(
    app: State<'_, App>,
    url: String,
) -> CmdResult<Option<BatchView>> {
    if !shardload_manager::batch::is_pattern(&url) {
        return Ok(None);
    }
    let urls = shardload_manager::batch::expand(&url)?;
    let known = known_links(app, urls.clone())
        .await?
        .into_iter()
        .filter(|k| *k)
        .count();
    Ok(Some(BatchView {
        count: urls.len(),
        first: urls.first().cloned().unwrap_or_default(),
        last: urls.last().cloned().unwrap_or_default(),
        known,
    }))
}

/// Adds every download a batch address stands for, each named by its server; with
/// `skip_known`, not those downloaded before or on the list. Returns how many were added.
#[tauri::command]
pub(crate) async fn add_batch(app: State<'_, App>, request: AddRequest) -> CmdResult<usize> {
    let mut urls = shardload_manager::batch::expand(&request.url)?;
    if request.skip_known {
        let known = known_links(app.clone(), urls.clone()).await?;
        let mut known = known.into_iter();
        urls.retain(|_| !known.next().unwrap_or(false));
    }
    remember_login(&app, &request.url, request.login.as_ref()).await?;
    let headers = request_headers(request.headers, request.login.as_ref());
    // Named after the pattern's file part: `photo[001-100].jpg`.
    let name = request
        .url
        .split(['?', '#'])
        .next()
        .and_then(|p| p.rsplit('/').next())
        .unwrap_or(&request.url)
        .to_owned();
    let group = match urls.len() {
        0 | 1 => None,
        _ => Some(app.manager.new_group(&name).await.map_err(msg)?.id),
    };
    for url in &urls {
        app.manager
            .add(NewTask {
                url: url.clone(),
                headers: headers.clone(),
                dir: request.dir.clone(),
                start: request.start,
                group,
                ..NewTask::default()
            })
            .await
            .map_err(msg)?;
    }
    Ok(urls.len())
}

/// What `ids` (and the running downloads) need on each drive, before a queue starts.
#[tauri::command]
pub(crate) async fn disk_needs(
    app: State<'_, App>,
    ids: Vec<TaskId>,
) -> CmdResult<Vec<serde_json::Value>> {
    let needs = app.manager.disk_needs(&ids).await.map_err(msg)?;
    Ok(needs.iter().map(drive_view).collect())
}

/// A drive's need with what it is short of.
pub(crate) fn drive_view(n: &shardload_manager::disk::DriveNeed) -> serde_json::Value {
    serde_json::json!({
        "volume": n.volume,
        "need": n.need,
        "free": n.free,
        "unknown": n.unknown,
        "tasks": n.tasks,
        "short": n.short(),
        "reserve": shardload_manager::disk::RESERVE,
    })
}
