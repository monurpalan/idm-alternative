//! Video pages and playlists through yt-dlp, groups, and the helper programs.

use super::*;

/// What a page of a site such as YouTube offers, read by yt-dlp with the page's headers.
#[tauri::command]
pub(crate) async fn inspect_page(
    app: State<'_, App>,
    url: String,
    headers: Option<Vec<(String, String)>>,
    whole_list: Option<bool>,
) -> CmdResult<shardload_manager::ytdlp::PageInfo> {
    let ytdlp = tools::installed(&app.data, tools::Tool::Ytdlp)
        .ok_or_else(|| "yt-dlp is not set up".to_owned())?;
    let tools = shardload_manager::ytdlp::Tools {
        ytdlp,
        deno: tools::installed(&app.data, tools::Tool::Deno),
        ffmpeg: app.manager.config().ffmpeg,
    };
    shardload_manager::ytdlp::inspect(
        &tools,
        &url,
        &headers.unwrap_or_default(),
        &app.data.join("tools").join("scratch"),
        whole_list.unwrap_or(false),
    )
    .await
}

/// Videos of a playlist or a channel to add, all at one quality.
#[derive(Deserialize)]
pub(crate) struct PlaylistRequest {
    /// The list's name: the group's, and the folder's the videos go into.
    pub(crate) title: String,
    pub(crate) entries: Vec<PlaylistItem>,
    /// Up to this height; `None` for audio alone.
    pub(crate) height: Option<u32>,
    #[serde(default)]
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) start: bool,
}

#[derive(Deserialize, Clone)]
pub(crate) struct PlaylistItem {
    pub(crate) url: String,
    pub(crate) title: Option<String>,
}

#[tauri::command]
pub(crate) async fn add_playlist(
    app: State<'_, App>,
    request: PlaylistRequest,
) -> CmdResult<usize> {
    Ok(add_videos(&app, request).await?.len())
}

/// Adds each video as a download of its own (through yt-dlp), in one group, into a folder
/// named after the list inside the video (or, for audio, music) folder.
pub(crate) async fn add_videos(app: &App, request: PlaylistRequest) -> CmdResult<Vec<Task>> {
    if tools::installed(&app.data, tools::Tool::Ytdlp).is_none() {
        return Err("yt-dlp is not set up".into());
    }
    let config = app.manager.config();
    let category = match request.height {
        Some(_) => shardload_manager::Category::Video,
        None => shardload_manager::Category::Music,
    };
    let base = config
        .category_dirs
        .get(&category)
        .unwrap_or(&config.dir)
        .clone();
    let title = shardload_engine::name::sanitize(request.title.trim());
    let dir = if title.is_empty() {
        base
    } else {
        base.join(&title)
    };
    let group = app.manager.new_group(&request.title).await.map_err(msg)?;
    let mut added = Vec::new();
    for item in request.entries {
        let task = app
            .manager
            .add(NewTask {
                url: item.url,
                headers: request.headers.clone(),
                dir: Some(dir.clone()),
                name: item.title.map(|t| format!("{t}.mp4")),
                start: request.start,
                stream: Some(shardload_manager::Stream {
                    ytdlp: Some(shardload_manager::ytdlp::selector(request.height)),
                    ..Default::default()
                }),
                group: Some(group.id),
                ..NewTask::default()
            })
            .await
            .map_err(msg)?;
        added.push(task);
    }
    Ok(added)
}

#[tauri::command]
pub(crate) async fn list_groups(app: State<'_, App>) -> CmdResult<Vec<shardload_manager::Group>> {
    app.manager.groups().await.map_err(msg)
}

#[tauri::command]
pub(crate) async fn new_group(
    app: State<'_, App>,
    name: String,
) -> CmdResult<shardload_manager::Group> {
    app.manager.new_group(&name).await.map_err(msg)
}

/// The first program `name` in a folder on PATH.
pub(crate) fn find_on_path(name: &str) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&name))
        .find(|p| p.is_file())
}

/// Which helper programs the app has, and their versions.
#[derive(Serialize)]
pub(crate) struct Tools {
    ffmpeg: Option<String>,
    /// The ffmpeg used is the one on PATH, not one the app set up.
    ffmpeg_on_path: bool,
    ytdlp: Option<String>,
    deno: Option<String>,
}

#[tauri::command]
pub(crate) async fn tools(app: State<'_, App>) -> CmdResult<Tools> {
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
    let ffmpeg = match &app.path_ffmpeg {
        Some(exe) => tools::version(exe).await.or_else(|| Some(String::new())),
        None => version(tools::Tool::Ffmpeg).await,
    };
    Ok(Tools {
        ffmpeg,
        ffmpeg_on_path: app.path_ffmpeg.is_some(),
        ytdlp: version(tools::Tool::Ytdlp).await,
        deno: version(tools::Tool::Deno).await,
    })
}

/// Sets up (or updates to the latest release) a helper program, on the user's request.
#[tauri::command]
pub(crate) async fn install_tool(app: State<'_, App>, tool: tools::Tool) -> CmdResult<String> {
    let opts = app.manager.config().engine;
    let cancel = shardload_engine::CancellationToken::new();
    let exe = tools::install(&app.data, tool, &opts, None, &cancel).await?;
    // Downloads started from now on use it.
    let settings = lock(&app.settings).clone();
    app.manager
        .set_config(manager_config(
            &settings,
            app.path_ffmpeg.clone(),
            &app.data,
        ))
        .await
        .map_err(msg)?;
    Ok(tools::version(&exe).await.unwrap_or_default())
}

/// One quality the button over a video lists on the page.
#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct Quality {
    /// What the dialog picks: a height or `audio` for a page, a variant's id for a stream.
    pub(crate) id: String,
    pub(crate) label: String,
}

/// The qualities the first item of `media` offers, best first, read the way the dialog
/// reads them (and kept for it). A page of a video site goes through yt-dlp; an HLS or DASH
/// playlist is read directly; a plain file or a playlist page offers nothing to choose.
pub(crate) async fn media_qualities(
    handle: &AppHandle,
    media: &shardload_bridge::MediaCapture,
) -> Result<Vec<Quality>, String> {
    let Some(item) = media.items.first() else {
        return Ok(Vec::new());
    };
    let headers = media.request_headers();
    let app = handle.state::<App>();
    let (qualities, info) = match item.kind.as_str() {
        "page" => {
            let info = inspect_page(app.clone(), item.url.clone(), Some(headers), None).await?;
            (page_qualities(&info), serde_json::to_value(&info))
        }
        "hls" | "dash" => {
            let info = inspect_media(app.clone(), item.url.clone(), Some(headers)).await?;
            (stream_qualities(&info), serde_json::to_value(&info))
        }
        _ => return Ok(Vec::new()),
    };
    if let Ok(info) = info {
        *lock(&app.media_info) = Some((item.url.clone(), info));
    }
    Ok(qualities)
}

fn page_qualities(info: &shardload_manager::ytdlp::PageInfo) -> Vec<Quality> {
    if info.playlist.is_some() {
        // Its videos are picked in the app.
        return Vec::new();
    }
    let mut out: Vec<Quality> = info
        .heights
        .iter()
        .map(|h| Quality {
            id: h.to_string(),
            label: format!("{h}p"),
        })
        .collect();
    if info.audio_only {
        out.push(Quality {
            id: "audio".into(),
            label: "Audio only".into(),
        });
    }
    out
}

fn stream_qualities(info: &shardload_media::Streams) -> Vec<Quality> {
    if info.video.len() < 2 {
        return Vec::new();
    }
    info.video
        .iter()
        .map(|v| {
            let mut parts = Vec::new();
            if let Some(h) = v.height {
                parts.push(format!("{h}p"));
            }
            if v.frame_rate.is_some_and(|f| f > 30.5) {
                parts.push(format!("{:.0} fps", v.frame_rate.unwrap_or_default()));
            }
            if v.hdr.is_some() {
                parts.push("HDR".into());
            }
            if v.bandwidth > 0 {
                parts.push(format!("{:.1} Mbps", v.bandwidth as f64 / 1e6));
            }
            Quality {
                id: v.id.clone(),
                label: if parts.is_empty() {
                    v.id.clone()
                } else {
                    parts.join(" · ")
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_video_page_lists_its_heights_and_audio() {
        let info: shardload_manager::ytdlp::PageInfo = shardload_manager::ytdlp::PageInfo {
            title: None,
            duration: None,
            heights: vec![1080, 720],
            audio_only: true,
            playlist: None,
            in_playlist: false,
            live: false,
        };
        let ids: Vec<_> = page_qualities(&info)
            .into_iter()
            .map(|q| (q.id, q.label))
            .collect();
        assert_eq!(
            ids,
            [
                ("1080".into(), "1080p".into()),
                ("720".into(), "720p".into()),
                ("audio".into(), "Audio only".into())
            ]
        );
    }
}
