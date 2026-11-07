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
    read_page(
        &app,
        &url,
        &headers.unwrap_or_default(),
        whole_list.unwrap_or(false),
        false,
    )
    .await
}

/// `inspect_page`, and with `known_only` only through an extractor made for the site.
async fn read_page(
    app: &App,
    url: &str,
    headers: &[(String, String)],
    whole_list: bool,
    known_only: bool,
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
        url,
        headers,
        &app.data.join("tools").join("scratch"),
        whole_list,
        known_only,
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
#[derive(Serialize, Debug, PartialEq, Clone)]
pub(crate) struct Quality {
    /// What the dialog picks: a height or `audio` for a page, a variant's id for a stream.
    pub(crate) id: String,
    pub(crate) label: String,
}

/// How many streams of a page are read (together) for the one listing qualities. A player
/// fetches a list per quality as it switches, newer than the master that names them all; the
/// extension puts the masters it recognised first, and the first with several qualities wins.
const STREAMS_TRIED: usize = 12;

/// The qualities `media` offers, best first, read the way the dialog reads them (and kept for
/// it), with the link of the item they are for. A page (the first item) goes through yt-dlp;
/// when what the tab fetched follows it (the post a video is in, on a site handled nowhere
/// else), only a site yt-dlp knows is read, and the streams are read meanwhile: the page's
/// answer when it has one, else the streams'. A plain file or a playlist page offers nothing
/// to choose.
pub(crate) async fn media_qualities(
    handle: &AppHandle,
    media: &shardload_bridge::MediaCapture,
) -> Result<(String, Vec<Quality>), String> {
    let Some(first) = media.items.first() else {
        return Ok((String::new(), Vec::new()));
    };
    let app = handle.state::<App>();
    let headers = media.request_headers();
    if first.kind != "page" {
        return streams_qualities(&app, &media.items, &headers).await;
    }
    let rest = &media.items[1..];
    if rest.is_empty() {
        return page_qualities_of(&app, &first.url, &headers, false).await;
    }
    let (page, streams) = tokio::join!(
        page_qualities_of(&app, &first.url, &headers, true),
        streams_qualities(&app, rest, &headers)
    );
    match (page, streams) {
        (Ok(page), _) => Ok(page),
        (Err(_), Ok(streams)) => Ok(streams),
        (Err(e), Err(_)) => Err(e),
    }
}

/// A page's qualities through yt-dlp, kept for the dialog.
async fn page_qualities_of(
    app: &App,
    url: &str,
    headers: &[(String, String)],
    known_only: bool,
) -> Result<(String, Vec<Quality>), String> {
    if let Some((_, qualities)) = lock(&app.media_info).get(url) {
        return Ok((url.to_owned(), qualities));
    }
    let info = read_page(app, url, headers, false, known_only).await?;
    let qualities = page_qualities(&info);
    if let Ok(info) = serde_json::to_value(&info) {
        lock(&app.media_info).put(url, info, &qualities);
    }
    Ok((url.to_owned(), qualities))
}

/// The HLS or DASH playlists among `items`, read together and kept for the dialog: the first
/// (in their order) with several qualities, else the first with a picture: a feed's newest
/// playlist may be audio alone. Nothing to choose when there are none (plain files).
async fn streams_qualities(
    app: &App,
    items: &[shardload_bridge::MediaItem],
    headers: &[(String, String)],
) -> Result<(String, Vec<Quality>), String> {
    let streams: Vec<&shardload_bridge::MediaItem> = items
        .iter()
        .filter(|i| matches!(i.kind.as_str(), "hls" | "dash"))
        .take(STREAMS_TRIED)
        .collect();
    let read = futures_util::future::join_all(streams.iter().map(|item| async move {
        if let Some((info, qualities)) = lock(&app.media_info).get(&item.url) {
            return Ok((pictures_in(&info), qualities));
        }
        let info = read_stream(app, &item.url, headers).await?;
        let qualities = stream_qualities(&info);
        if let Ok(value) = serde_json::to_value(&info) {
            lock(&app.media_info).put(&item.url, value, &qualities);
        }
        Ok::<_, String>((info.video.len(), qualities))
    }))
    .await;
    let counts: Vec<usize> = read
        .iter()
        .map(|r| r.as_ref().map_or(0, |(n, _)| *n))
        .collect();
    if let Some(i) = choose_stream(&counts) {
        let Ok((_, qualities)) = &read[i] else {
            unreachable!("a stream that failed counts no pictures")
        };
        return Ok((streams[i].url.clone(), qualities.clone()));
    }
    // None read with a picture: why the first failed, when every one did; else nothing to
    // choose (audio alone).
    let all_failed = !read.is_empty() && read.iter().all(Result::is_err);
    match read.into_iter().find_map(Result::err) {
        Some(e) if all_failed => Err(e),
        _ => Ok((
            items.first().map(|i| i.url.clone()).unwrap_or_default(),
            Vec::new(),
        )),
    }
}

/// How many video qualities a kept reading of a stream lists.
fn pictures_in(info: &serde_json::Value) -> usize {
    info.get("video")
        .and_then(|v| v.as_array())
        .map_or(0, Vec::len)
}

/// Which of the streams read (by how many video qualities each lists) the button offers: the
/// first with several, else the first with a picture at all.
fn choose_stream(pictures: &[usize]) -> Option<usize> {
    pictures
        .iter()
        .position(|n| *n >= 2)
        .or_else(|| pictures.iter().position(|n| *n >= 1))
}

/// What was read for the latest few videos, for a while: the links in it expire.
#[derive(Default)]
pub(crate) struct MediaCache {
    entries: Vec<(String, std::time::Instant, serde_json::Value, Vec<Quality>)>,
}

impl MediaCache {
    const KEEP: usize = 16;
    const FOR: std::time::Duration = std::time::Duration::from_secs(15 * 60);

    pub(crate) fn get(&self, url: &str) -> Option<(serde_json::Value, Vec<Quality>)> {
        self.entries
            .iter()
            .find(|(u, at, ..)| u == url && at.elapsed() < Self::FOR)
            .map(|(_, _, info, q)| (info.clone(), q.clone()))
    }

    fn put(&mut self, url: &str, info: serde_json::Value, qualities: &[Quality]) {
        self.entries
            .retain(|(u, at, ..)| u != url && at.elapsed() < Self::FOR);
        self.entries.push((
            url.to_owned(),
            std::time::Instant::now(),
            info,
            qualities.to_vec(),
        ));
        if self.entries.len() > Self::KEEP {
            self.entries.remove(0);
        }
    }
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
    fn the_stream_with_several_qualities_is_offered_before_audio_alone() {
        assert_eq!(choose_stream(&[]), None);
        assert_eq!(choose_stream(&[0, 0]), None);
        // An audio playlist lists no video; a media playlist lists one.
        assert_eq!(choose_stream(&[0, 1]), Some(1));
        assert_eq!(choose_stream(&[1, 0, 3]), Some(2));
        assert_eq!(choose_stream(&[0, 4, 2]), Some(1));
        assert_eq!(
            pictures_in(&serde_json::json!({ "video": [{}, {}], "audio": [] })),
            2
        );
        assert_eq!(pictures_in(&serde_json::json!({})), 0);
    }

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
