//! Downloading streams (HLS, DASH) and files out of ZIP archives.

use super::*;

impl Inner {
    /// A stream (HLS or DASH): the name comes from the page title (or the playlist URL) with
    /// the extension of what will be written: `.mp4` with ffmpeg or for DASH (fragmented
    /// MP4), `.ts` for HLS without ffmpeg.
    #[allow(
        clippy::too_many_lines,
        clippy::cognitive_complexity,
        reason = "one long sequence kept together; to be split when it next changes"
    )]
    pub(crate) async fn download_stream(
        &self,
        mut task: Task,
        stream: Stream,
        opts: Options,
        conflict: Conflict,
        ffmpeg: Option<PathBuf>,
        cancel: &CancellationToken,
    ) -> Result<Outcome> {
        let path = match &task.path {
            Some(p) => p.clone(),
            None => {
                std::fs::create_dir_all(&task.dir).map_err(shardload_engine::Error::from)?;
                let base = task
                    .name
                    .clone()
                    .or_else(|| shardload_engine::name::from_url(&task.url))
                    .unwrap_or_else(|| "video".to_owned());
                let stem = clip_stem(shardload_engine::name::split_extension(&base).0, &stream);
                let ext = if ffmpeg.is_some() || stream.dash.is_some() {
                    "mp4"
                } else {
                    "ts"
                };
                let name = shardload_engine::name::sanitize(&format!("{stem}.{ext}"));
                let _one_at_a_time = self.naming.lock().await;
                let taken: std::collections::HashSet<PathBuf> =
                    self.db.paths_except(task.id).await?.into_iter().collect();
                let path =
                    shardload_engine::target_avoiding(&task.dir, &name, &task.url, conflict, |p| {
                        taken.contains(p)
                    });
                let file_name = path
                    .file_name()
                    .map_or_else(|| name.clone(), |n| n.to_string_lossy().into_owned());
                self.db
                    .set_target(task.id, &file_name, &path, None, Category::Video)
                    .await?;
                task.name = Some(file_name);
                task.path = Some(path.clone());
                task.category = Category::Video;
                self.emit(Event::Updated(task.clone()));
                path
            }
        };
        // A clip's subtitles would not match its times.
        let subtitles = match stream.clip {
            Some(_) => Vec::new(),
            None => stream.subtitles.clone(),
        };
        let clip = stream.clip;
        let is_dash = stream.dash.is_some();
        let video_path = path.clone();
        let (tx, rx) = watch::channel(Progress::default());
        let reporter = tokio::spawn(report(
            task.id,
            rx,
            self.events.clone(),
            self.db.clone(),
            None,
            hints::Watch::default(),
        ));
        if let Some(record) = &stream.record {
            let limit = record.limit.map(Duration::from_secs);
            if let Some(video) = stream.dash {
                let req = shardload_engine::DashRequest {
                    manifest: task.url.clone(),
                    video,
                    audio: stream.audio,
                    output: path,
                    headers: task.headers.clone(),
                    ffmpeg,
                    clip: None,
                };
                let result =
                    shardload_engine::record_dash(&req, limit, &opts, Some(&tx), cancel).await;
                drop(tx);
                reporter.await.ok();
                return Ok(result?);
            }
            let req = shardload_engine::RecordRequest {
                video: task.url.clone(),
                audio: stream.audio,
                output: path,
                headers: task.headers.clone(),
                ffmpeg,
                limit,
            };
            let result = shardload_engine::record_hls(&req, &opts, Some(&tx), cancel).await;
            drop(tx);
            reporter.await.ok();
            return Ok(result?);
        }
        let result = match stream.dash {
            Some(video) => {
                let req = shardload_engine::DashRequest {
                    manifest: task.url.clone(),
                    video,
                    audio: stream.audio,
                    output: path,
                    headers: task.headers.clone(),
                    ffmpeg,
                    clip,
                };
                shardload_engine::download_dash(&req, &opts, Some(&tx), cancel).await
            }
            None => {
                let req = shardload_engine::StreamRequest {
                    video: task.url.clone(),
                    audio: stream.audio,
                    output: path,
                    headers: task.headers.clone(),
                    ffmpeg,
                    clip,
                };
                shardload_engine::download_hls(&req, &opts, Some(&tx), cancel).await
            }
        };
        drop(tx);
        reporter.await.ok();
        let outcome = result?;
        // Subtitles go next to the video; a subtitle that fails does not fail the download.
        for (i, sub) in subtitles.iter().enumerate() {
            let source = if is_dash {
                shardload_engine::SubtitleSource::Dash {
                    manifest: task.url.clone(),
                    representation: sub.id.clone(),
                }
            } else {
                shardload_engine::SubtitleSource::Hls {
                    playlist: sub.id.clone(),
                }
            };
            let label = sub.language.clone().unwrap_or_else(|| (i + 1).to_string());
            let out = subtitle_path(&video_path, &label, "vtt");
            match shardload_engine::download_subtitles(&source, &out, &task.headers, &opts).await {
                Ok(_) => {
                    // A DASH text track can be TTML rather than WebVTT.
                    let is_xml = std::fs::read(&out)
                        .map(|b| b.iter().find(|c| !c.is_ascii_whitespace()) == Some(&b'<'))
                        .unwrap_or(false);
                    if is_xml {
                        std::fs::rename(&out, subtitle_path(&video_path, &label, "ttml")).ok();
                    }
                }
                Err(e) => tracing::warn!(id = task.id, "subtitles {label} not saved: {e}"),
            }
        }
        Ok(outcome)
    }

    /// Some files out of a ZIP archive: on the first start a folder named after the archive
    /// is picked for them, and kept. Files already there with the right size are not fetched
    /// again, so a stopped download goes on where it was.
    pub(crate) async fn download_archive(
        &self,
        mut task: Task,
        mut stream: Stream,
        mut choice: ArchiveChoice,
        opts: &Options,
        conflict: Conflict,
        cancel: &CancellationToken,
    ) -> Result<Outcome> {
        let started = std::time::Instant::now();
        let zip = tokio::select! {
            z = shardload_engine::RemoteZip::open(&task.url, &task.headers, opts) => z?,
            () = cancel.cancelled() => return Err(shardload_engine::Error::Cancelled.into()),
        };
        let (chosen, missing) =
            shardload_engine::select_zip_entries(zip.entries(), &choice.entries);
        if !missing.is_empty() || chosen.is_empty() {
            return Err(shardload_engine::Error::Archive(format!(
                "not in the archive: {}",
                missing.join(", ")
            ))
            .into());
        }
        let size: u64 = chosen.iter().map(|&i| zip.entries()[i].size).sum();
        let folder = match choice.folder.clone() {
            Some(f) => f,
            None => {
                std::fs::create_dir_all(&task.dir).map_err(shardload_engine::Error::from)?;
                let _one_at_a_time = self.naming.lock().await;
                let taken: std::collections::HashSet<PathBuf> =
                    self.db.paths_except(task.id).await?.into_iter().collect();
                let base = shardload_engine::name::sanitize(
                    shardload_engine::name::split_extension(zip.name()).0,
                );
                let folder =
                    shardload_engine::target_avoiding(&task.dir, &base, &task.url, conflict, |p| {
                        taken.contains(p)
                    });
                let name = folder
                    .file_name()
                    .map_or_else(|| base.clone(), |n| n.to_string_lossy().into_owned());
                let files: Vec<&str> = chosen
                    .iter()
                    .map(|&i| zip.entries()[i].name.as_str())
                    .filter(|n| !n.ends_with('/'))
                    .collect();
                let category = match files.as_slice() {
                    [one] => Category::for_name(one),
                    _ => Category::for_name(zip.name()),
                };
                self.db
                    .set_target(task.id, &name, &folder, Some(size), category)
                    .await?;
                choice.folder = Some(folder.clone());
                stream.archive = Some(choice.clone());
                self.db.set_stream(task.id, &stream).await?;
                task.name = Some(name);
                task.path = Some(folder.clone());
                task.size = Some(size);
                task.category = category;
                task.stream = Some(stream);
                self.emit(Event::Updated(task.clone()));
                folder
            }
        };
        let (tx, rx) = watch::channel(Progress::default());
        let reporter = tokio::spawn(report(
            task.id,
            rx,
            self.events.clone(),
            self.db.clone(),
            None,
            hints::Watch::default(),
        ));
        let result = zip
            .extract(&chosen, &folder, false, Some(&tx), cancel)
            .await;
        drop(tx);
        reporter.await.ok();
        let done = result?;
        let failed: Vec<String> = done
            .iter()
            .filter_map(|d| d.error.as_ref().map(|e| format!("{}: {e}", d.name)))
            .collect();
        if !failed.is_empty() {
            return Err(shardload_engine::Error::Archive(format!(
                "{} of {} files could not be extracted: {}",
                failed.len(),
                done.len(),
                failed.join("; ")
            ))
            .into());
        }
        if opts.mark_of_the_web {
            for path in done.iter().filter_map(|d| d.path.as_ref()) {
                if path.is_file() {
                    shardload_engine::mark_of_the_web(path, &task.url, &task.headers);
                }
            }
        }
        Ok(Outcome {
            size,
            resumed_from: 0,
            elapsed: started.elapsed(),
            checksum: None,
            checksum_from: None,
        })
    }
}
