//! What the bridge's tool requests do: adding downloads, playlists, where a file came
//! from, waiting for downloads, and how a download is shown to them.

use super::*;

/// A download added by a local tool: a file, a page of a video site (through yt-dlp, with
/// `video_height` or `audio_only`), a torrent (with `torrent_files`), or files out of a ZIP
/// archive (with `archive_entries`).
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "one long sequence kept together; to be split when it next changes"
)]
pub(crate) async fn tool_add(
    handle: &AppHandle,
    add: shardload_bridge::AddDownload,
) -> CmdResult<Task> {
    let app = handle.state::<App>();
    let headers = shardload_manager::command::parse_header_lines(&add.headers.join("\n"))?;
    let mut name = add.name.filter(|n| !n.trim().is_empty());
    let mut stream = None;
    let clip_asked = add.clip_start.is_some() || add.clip_end.is_some();
    let record = (add.record == Some(true)).then(|| shardload_manager::Recording {
        limit: add.record_minutes.filter(|m| *m > 0).map(|m| m * 60),
        from_start: add.record_from_start == Some(true),
        start_at: add.record_in_minutes.filter(|m| *m > 0).map(|m| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64)
                + m as i64 * 60_000
        }),
    });
    if record.is_some() && add.video_height.is_none() && add.audio_only != Some(true) {
        return Err("record is for live video pages: give video_height or audio_only too".into());
    }
    if clip_asked && add.video_height.is_none() && add.audio_only != Some(true) {
        return Err(
            "clip_start and clip_end are for video pages: give video_height or audio_only too"
                .into(),
        );
    }
    if add.video_height.is_some() || add.audio_only == Some(true) {
        let height = if add.audio_only == Some(true) {
            None
        } else {
            add.video_height
        };
        let mut length = None;
        if name.is_none() || clip_asked {
            // The page's title, as the media dialog does, and its length for a clip.
            let info = inspect_page(handle.state::<App>(), add.url.clone(), None, None).await?;
            length = info.duration;
            name = name.or(info.title.map(|t| format!("{t}.mp4")));
        } else if tools::installed(&app.data, tools::Tool::Ytdlp).is_none() {
            return Err("yt-dlp is not set up".into());
        }
        let clip = match clip_asked {
            true => Some(shardload_manager::clip::from_times(
                add.clip_start.as_deref().unwrap_or("0"),
                add.clip_end
                    .as_deref()
                    .ok_or("give clip_end too: where the clip stops")?,
                add.exact_cut == Some(true),
                length,
            )?),
            false => None,
        };
        stream = Some(shardload_manager::Stream {
            ytdlp: Some(shardload_manager::ytdlp::selector(height)),
            clip,
            record,
            ..Default::default()
        });
    } else if let Some(files) = add.torrent_files {
        stream = Some(shardload_manager::Stream {
            torrent: Some(shardload_manager::TorrentChoice {
                files: Some(files),
                folder: None,
            }),
            ..Default::default()
        });
    } else if let Some(entries) = add.archive_entries.filter(|e| !e.is_empty()) {
        stream = Some(shardload_manager::Stream {
            archive: Some(shardload_manager::ArchiveChoice {
                entries,
                ..Default::default()
            }),
            ..Default::default()
        });
    }
    // A tool may save only into the app's own folders (or inside them), so a request cannot
    // put a file anywhere else, such as an autostart folder.
    let dir = add.dir.filter(|d| !d.trim().is_empty()).map(PathBuf::from);
    if let Some(dir) = &dir {
        let folders = app.manager.download_folders();
        if !shardload_manager::folder::within(dir, &folders) {
            let allowed: Vec<String> = folders.iter().map(|f| f.display().to_string()).collect();
            return Err(format!(
                "{} is not a download folder or inside one; use one of: {}",
                dir.display(),
                allowed.join(", ")
            ));
        }
    }
    if add.allow_duplicate != Some(true) {
        // A plain link is asked about first, so a file of the same name and size counts too.
        let plain = stream.is_none()
            && !shardload_engine::metalink::is_metalink(&add.url)
            && !shardload_manager::torrent::is_torrent(&add.url);
        let probe = match plain {
            true => app.manager.probe(&add.url, &[]).await.ok(),
            false => None,
        };
        let previous = app
            .manager
            .find_previous(
                &add.url,
                probe.as_ref().map(|p| p.url.as_str()),
                probe.as_ref().map(|p| p.name.as_str()),
                probe.as_ref().and_then(|p| p.size),
            )
            .await
            .map_err(msg)?;
        if let Some(p) = previous {
            return Err(already_downloaded(&p));
        }
    }
    app.manager
        .add(NewTask {
            mirrors: clean_mirrors(add.mirrors),
            url: add.url,
            name,
            dir,
            start: add.start.unwrap_or(true),
            checksum: add.checksum.filter(|c| !c.trim().is_empty()),
            pieces_from: add.pieces_from.filter(|p| !p.trim().is_empty()),
            page_checksum: add.page_checksum.filter(|c| !c.trim().is_empty()),
            headers,
            stream,
            ..NewTask::default()
        })
        .await
        .map_err(msg)
}

/// Videos of a playlist or a channel added by a local tool: `playlist_items` are their
/// numbers in inspect_video's list (from 1). Ones downloaded before are left out unless
/// `allow_duplicate`. Answers `{group, added, skipped}`.
pub(crate) async fn tool_add_playlist(
    handle: &AppHandle,
    add: shardload_bridge::AddDownload,
) -> CmdResult<serde_json::Value> {
    let app = handle.state::<App>();
    let wanted = add.playlist_items.unwrap_or_default();
    if wanted.is_empty() {
        return Err("playlist_items is empty".into());
    }
    let info = inspect_page(handle.state::<App>(), add.url.clone(), None, Some(true)).await?;
    let list = info
        .playlist
        .ok_or("this page is not a playlist or a channel; leave out playlist_items")?;
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    for n in wanted {
        let entry = n
            .checked_sub(1)
            .and_then(|i| list.entries.get(i))
            .ok_or_else(|| {
                format!(
                    "the list has {} videos; {n} is not one of them",
                    list.entries.len()
                )
            })?;
        let known = app
            .manager
            .find_previous(&entry.url, None, None, None)
            .await
            .map_err(msg)?
            .is_some();
        if known && add.allow_duplicate != Some(true) {
            skipped.push(entry.url.clone());
        } else {
            entries.push(PlaylistItem {
                url: entry.url.clone(),
                title: entry.title.clone(),
            });
        }
    }
    let title = add
        .name
        .filter(|n| !n.trim().is_empty())
        .or(list.title)
        .unwrap_or_else(|| "Playlist".into());
    let height = match add.audio_only {
        Some(true) => None,
        _ => Some(add.video_height.unwrap_or(1080)),
    };
    let added = if entries.is_empty() {
        Vec::new()
    } else {
        add_videos(
            &app,
            PlaylistRequest {
                title: title.clone(),
                entries,
                height,
                headers: Vec::new(),
                start: add.start.unwrap_or(true),
            },
        )
        .await?
    };
    Ok(serde_json::json!({
        "group": title,
        "added": added.iter().map(|t| task_view(t, None)).collect::<Vec<_>>(),
        "skipped_downloaded_before": skipped,
    }))
}

/// Where a file came from, for a local tool; with the checks it asked for.
pub(crate) async fn tool_file_origin(
    handle: &AppHandle,
    path: &str,
    check_checksum: Option<bool>,
    check_update: Option<bool>,
) -> CmdResult<serde_json::Value> {
    let app = handle.state::<App>();
    let origin = app
        .manager
        .file_origin(Path::new(path.trim()))
        .await
        .map_err(msg)?;
    let checksum = match check_checksum {
        Some(true) => Some(
            app.manager
                .check_file_checksum(
                    &origin.path,
                    origin.checksum.as_deref(),
                    origin.url.as_deref(),
                )
                .await
                .map_err(msg)?,
        ),
        _ => None,
    };
    let update = match (check_update, &origin.url) {
        (Some(true), Some(url)) => Some(
            app.manager
                .check_for_update(url, origin.size, origin.downloaded_at)
                .await
                .map_err(msg)?,
        ),
        _ => None,
    };
    Ok(serde_json::json!({
        "path": origin.path,
        "size": origin.size,
        "traced": origin.traced,
        "url": origin.url,
        "page": origin.page,
        "downloaded": origin.downloaded_at.map(utc_date),
        "checksum": origin.checksum,
        "download_id": origin.history.as_ref().and_then(|h| h.task_id),
        "checksum_check": checksum,
        "update_check": update,
    }))
}

/// Why a tool's download was not added: what was found, and how to add it anyway.
pub(crate) fn already_downloaded(p: &shardload_manager::Previous) -> String {
    let found = serde_json::json!({
        "id": p.id,
        "status": p.status,
        "name": p.name,
        "path": p.path,
        "size": p.size,
        "finished": p.finished_at.map(utc_date),
        "file_still_there": p.on_disk,
        "matched": p.matched,
    });
    let what = if p.unfinished() {
        "already on the list, not finished yet"
    } else if p.on_disk {
        "downloaded before, and the file is still there"
    } else {
        "downloaded before, but the file is no longer where it was saved"
    };
    format!("{what}; pass allow_duplicate: true to add it again. Found: {found}")
}

/// `2026-10-03` for Unix milliseconds (UTC).
pub(crate) fn utc_date(ms: i64) -> String {
    // Howard Hinnant's days-to-civil.
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Waits until none of `ids` is queued or downloading, or `timeout` passes, and returns
/// `{finished, downloads}`.
pub(crate) async fn wait_for(
    handle: &AppHandle,
    ids: &[TaskId],
    timeout: std::time::Duration,
) -> CmdResult<serde_json::Value> {
    let app = handle.state::<App>();
    let mut events = app.manager.subscribe();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let mut tasks = Vec::new();
        for id in ids {
            tasks.push(app.manager.get(*id).await.map_err(msg)?);
        }
        let finished = tasks
            .iter()
            .all(|t| !matches!(t.status, Status::Queued | Status::Downloading));
        let now = tokio::time::Instant::now();
        if finished || now >= deadline {
            let live = lock(&app.live).clone();
            return Ok(serde_json::json!({
                "finished": finished,
                "downloads": tasks.iter().map(|t| task_view(t, live.get(&t.id))).collect::<Vec<_>>(),
            }));
        }
        // Any change to the list may be one of ours; progress events are not waited for.
        let _ = tokio::time::timeout(deadline - now, async {
            while let Ok(Event::Progress(_)) = events.recv().await {}
        })
        .await;
    }
}

/// Runs `action` on each download, stopping at the first that fails.
pub(crate) async fn for_each<F, Fut>(
    handle: &AppHandle,
    ids: Vec<TaskId>,
    action: F,
) -> shardload_bridge::Reply
where
    F: Fn(Manager, TaskId) -> Fut,
    Fut: std::future::Future<Output = shardload_manager::Result<()>>,
{
    let manager = handle.state::<App>().manager.clone();
    for id in ids {
        if let Err(e) = action(manager.clone(), id).await {
            return shardload_bridge::Reply::error(format!("download {id}: {e}"));
        }
    }
    shardload_bridge::Reply::ok()
}

/// A download as local tools see it: no request headers, which may hold the browser's
/// cookies.
pub(crate) fn task_view(t: &Task, live: Option<&Live>) -> serde_json::Value {
    let (received, speed, eta, hints) = live.map_or((t.received, 0.0, None, &[][..]), |l| {
        (l.received, l.speed, l.eta, l.hints.as_slice())
    });
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
        "queue_pos": t.queue_pos,
        "speed_limit": t.speed_limit,
        "checksum": t.checksum,
        "checksum_from": t.checksum_from,
        "group_id": t.group_id,
        "content": t.content,
        "warning": t.warning,
        "eta_seconds": eta.map(|[soonest, latest]| [soonest.round(), latest.round()]),
        "hints": hints,
    })
}
