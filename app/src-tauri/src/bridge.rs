//! Requests from the browser extension and local tools (MCP, the CLI) over the bridge.

use super::*;

/// Answers the browser extension (through the native host): pings and hellos, and downloads
/// or link lists it hands over, which open in the window for the user to confirm.
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "one long sequence kept together; to be split when it next changes"
)]
pub(crate) async fn bridge_reply(
    handle: &AppHandle,
    request: shardload_bridge::Request,
) -> shardload_bridge::Reply {
    use shardload_bridge::{Reply, Request};
    if matches!(
        request,
        Request::Download(_) | Request::Links { .. } | Request::Media(_)
    ) {
        // Only the extension sends these; one too old to say hello still works.
        update_extensions(handle, |r| r.unnamed_at = Some(extensions::now())).await;
    }
    match request {
        Request::Ping => Reply {
            version: Some(env!("CARGO_PKG_VERSION").into()),
            ..Reply::ok()
        },
        Request::Extension(hello) => {
            update_extensions(handle, |r| r.heard(&hello, extensions::now())).await;
            Reply {
                version: Some(env!("CARGO_PKG_VERSION").into()),
                data: Some(serde_json::json!({ "protocol": shardload_bridge::PROTOCOL })),
                ..Reply::ok()
            }
        }
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
                        to_page(
                            handle,
                            "notice",
                            i18n::t(
                                lang(handle),
                                "Link refreshed: {name} continues",
                                &[("name", &name)],
                            ),
                            false,
                        );
                        return Reply::ok();
                    }
                    Err(e) => {
                        tracing::info!("captured link is not the awaited file: {e}");
                        to_page(
                            handle,
                            "notice",
                            i18n::t(
                                lang(handle),
                                "That link leads to a different file, so it opens as a new download",
                                &[],
                            ),
                            false,
                        );
                    }
                }
            }
            to_page(
                handle,
                "capture",
                serde_json::json!({
                    "url": capture.url,
                    "filename": capture.filename,
                    "size": capture.size,
                    "title": capture.title,
                    "checksum": capture.checksum,
                    "headers": capture.request_headers(),
                }),
                true,
            );
            Reply::ok()
        }
        Request::Links { referrer, links } => {
            to_page(
                handle,
                "capture-links",
                serde_json::json!({ "referrer": referrer, "links": links }),
                true,
            );
            Reply::ok()
        }
        Request::Rescue(r) => rescue(handle, r).await,
        Request::Media(media) => {
            to_page(
                handle,
                "capture-media",
                serde_json::json!({
                    "page": media.page,
                    "title": media.title,
                    "items": media.items,
                    "headers": media.request_headers(),
                }),
                true,
            );
            Reply::ok()
        }
        Request::Tasks => {
            let app = handle.state::<App>();
            match app.manager.list().await {
                Ok(tasks) => {
                    let live = lock(&app.live).clone();
                    Reply {
                        data: Some(
                            tasks
                                .iter()
                                .map(|t| task_view(t, live.get(&t.id)))
                                .collect(),
                        ),
                        ..Reply::ok()
                    }
                }
                Err(e) => Reply::error(e.to_string()),
            }
        }
        Request::Add(add) if add.playlist_items.is_some() => {
            match tool_add_playlist(handle, add).await {
                Ok(data) => Reply {
                    data: Some(data),
                    ..Reply::ok()
                },
                Err(e) => Reply::error(e),
            }
        }
        Request::Add(add) => match tool_add(handle, add).await {
            Ok(t) => Reply {
                data: Some(task_view(&t, None)),
                ..Reply::ok()
            },
            Err(e) => Reply::error(e),
        },
        Request::InspectVideo { url, whole_list } => {
            let app = handle.state::<App>();
            match inspect_page(app, url, None, whole_list).await {
                Ok(info) => Reply {
                    data: serde_json::to_value(info).ok(),
                    ..Reply::ok()
                },
                Err(e) => Reply::error(e),
            }
        }
        Request::InspectTorrent { url } => {
            let app = handle.state::<App>();
            match app.manager.inspect_torrent(&url).await {
                Ok(info) => Reply {
                    data: serde_json::to_value(info).ok(),
                    ..Reply::ok()
                },
                Err(e) => Reply::error(e.to_string()),
            }
        }
        Request::InspectArchive { url } => {
            let app = handle.state::<App>();
            match app.manager.inspect_archive(&url, &[]).await {
                Ok(info) => Reply {
                    data: serde_json::to_value(info).ok(),
                    ..Reply::ok()
                },
                Err(e) => Reply::error(e.to_string()),
            }
        }
        Request::Wait { ids, timeout_secs } => {
            let timeout = std::time::Duration::from_secs(timeout_secs.unwrap_or(60).clamp(1, 600));
            match wait_for(handle, &ids, timeout).await {
                Ok(data) => Reply {
                    data: Some(data),
                    ..Reply::ok()
                },
                Err(e) => Reply::error(e),
            }
        }
        Request::SpeedLimit {
            id,
            bytes_per_second,
        } => {
            let app = handle.state::<App>();
            let rate = bytes_per_second.filter(|r| *r > 0);
            let result = match id {
                Some(id) => app.manager.set_speed_limit(id, rate).await.map_err(msg),
                None => {
                    // Through the settings, so the window and the tray show it too.
                    let mut settings = lock(&app.settings).clone();
                    settings.speed_limit = rate;
                    apply_settings(handle, settings).await.map(drop)
                }
            };
            match result {
                Ok(()) => Reply::ok(),
                Err(e) => Reply::error(e),
            }
        }
        Request::Move { id, to } => {
            let app = handle.state::<App>();
            match serde_json::from_value::<Move>(serde_json::Value::String(to.clone())) {
                Ok(to) => match app.manager.move_task(id, to).await {
                    Ok(()) => Reply::ok(),
                    Err(e) => Reply::error(e.to_string()),
                },
                Err(_) => Reply::error(format!("`{to}` is not top, up, down or bottom")),
            }
        }
        Request::Rename { id, name } => {
            let app = handle.state::<App>();
            if name.trim().is_empty() {
                return Reply::error("the name is empty");
            }
            match app.manager.rename(id, &name).await {
                Ok(t) => Reply {
                    data: Some(task_view(&t, None)),
                    ..Reply::ok()
                },
                Err(e) => Reply::error(e.to_string()),
            }
        }
        Request::Relocate { id, dir } => {
            let app = handle.state::<App>();
            // Like adding: a tool may move files only into the app's own folders.
            let dir = PathBuf::from(dir);
            let folders = app.manager.download_folders();
            if !shardload_manager::folder::within(&dir, &folders) {
                let allowed: Vec<String> =
                    folders.iter().map(|f| f.display().to_string()).collect();
                return Reply::error(format!(
                    "{} is not a download folder or inside one; use one of: {}",
                    dir.display(),
                    allowed.join(", ")
                ));
            }
            match app.manager.relocate(id, &dir).await {
                Ok(t) => Reply {
                    data: Some(task_view(&t, None)),
                    ..Reply::ok()
                },
                Err(e) => Reply::error(e.to_string()),
            }
        }
        Request::DiskSpace => {
            let app = handle.state::<App>();
            let ids: Vec<TaskId> = match app.manager.list().await {
                Ok(tasks) => tasks.iter().map(|t| t.id).collect(),
                Err(e) => return Reply::error(e.to_string()),
            };
            match app.manager.disk_needs(&ids).await {
                Ok(needs) => Reply {
                    data: Some(needs.iter().map(drive_view).collect()),
                    ..Reply::ok()
                },
                Err(e) => Reply::error(e.to_string()),
            }
        }
        Request::Sites => {
            let app = handle.state::<App>();
            match app.manager.sites().await {
                Ok(sites) => Reply {
                    data: serde_json::to_value(sites).ok(),
                    ..Reply::ok()
                },
                Err(e) => Reply::error(e.to_string()),
            }
        }
        Request::FileOrigin {
            path,
            check_checksum,
            check_update,
        } => match tool_file_origin(handle, &path, check_checksum, check_update).await {
            Ok(data) => Reply {
                data: Some(data),
                ..Reply::ok()
            },
            Err(e) => Reply::error(e),
        },
        Request::ForgetSite { site } => {
            let app = handle.state::<App>();
            match app.manager.forget_site(site.trim()).await {
                Ok(()) => Reply::ok(),
                Err(e) => Reply::error(e.to_string()),
            }
        }
        Request::WhenDone { action } => {
            match serde_json::from_value::<schedule::WhenDone>(serde_json::Value::String(
                action.clone(),
            )) {
                Ok(action) => match arm_when_done(handle, action).await {
                    Ok(()) => Reply::ok(),
                    Err(e) => Reply::error(e),
                },
                Err(_) => Reply::error(format!(
                    "`{action}` is not nothing, exit, sleep, hibernate or shutdown"
                )),
            }
        }
        Request::Resume { ids } => {
            for_each(handle, ids, |m, id| async move { m.start(id).await }).await
        }
        Request::Pause { ids } => {
            for_each(handle, ids, |m, id| async move { m.pause(id).await }).await
        }
        Request::Remove { ids, delete_files } => {
            for_each(handle, ids, move |m, id| async move {
                m.remove(id, delete_files).await
            })
            .await
        }
        Request::Grab {
            url,
            depth,
            same_site,
        } => {
            let opts = shardload_manager::grabber::GrabOptions {
                depth: depth.unwrap_or(1).clamp(1, 5),
                same_site: same_site.unwrap_or(true),
                ..Default::default()
            };
            let manager = handle.state::<App>().manager.clone();
            let cancel = shardload_engine::CancellationToken::new();
            match manager.grab_site(&url, &opts, |_| {}, &cancel).await {
                Ok(found) => Reply {
                    data: serde_json::to_value(found).ok(),
                    ..Reply::ok()
                },
                Err(e) => Reply::error(e.to_string()),
            }
        }
        Request::Hello { .. } => Reply::error("unexpected hello"),
    }
}

/// Continues a download the browser left interrupted, keeping what the browser had when the
/// server still serves the same file, and says so in the window.
async fn rescue(handle: &AppHandle, r: shardload_bridge::Rescue) -> shardload_bridge::Reply {
    use shardload_bridge::Reply;
    let manager = handle.state::<App>().manager.clone();
    let rescue = shardload_manager::rescue::Rescue {
        headers: r.capture.request_headers(),
        url: r.capture.url,
        target: r.target.into(),
        received: r.received,
        size: r.capture.size,
    };
    match manager.rescue(rescue).await {
        Ok(done) => {
            let name = done.task.name.clone().unwrap_or_default();
            let lang = lang(handle);
            let text = if done.kept > 0 {
                i18n::t(
                    lang,
                    "{name} continues from where the browser stopped",
                    &[("name", &name)],
                )
            } else {
                let why = i18n::t(lang, done.why.as_deref().unwrap_or_default(), &[]);
                i18n::t(
                    lang,
                    "{name} starts over: {why}",
                    &[("name", &name), ("why", &why)],
                )
            };
            to_page(handle, "notice", text, true);
            Reply {
                data: Some(serde_json::json!({ "id": done.task.id, "kept": done.kept })),
                ..Reply::ok()
            }
        }
        Err(e) => Reply::error(e.to_string()),
    }
}
