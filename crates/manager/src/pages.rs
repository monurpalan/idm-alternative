//! Downloading torrents and pages of video sites (through yt-dlp).

use super::*;

impl Inner {
    /// A torrent: on the first start its files are listed (for a magnet link, asked from
    /// peers) to pick where they go: a single file straight into the download folder, several
    /// into a folder named after the torrent. The choice is kept for later starts.
    pub(crate) async fn download_torrent(
        &self,
        mut task: Task,
        conflict: Conflict,
        limit: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<Outcome> {
        let started = std::time::Instant::now();
        let state = |e: String| -> Error { shardload_engine::Error::State(e).into() };
        let mut stream = task.stream.clone().unwrap_or_default();
        let mut choice = stream.torrent.clone().unwrap_or_default();
        let folder = match choice.folder.clone() {
            Some(f) => f,
            None => {
                let info = tokio::select! {
                    i = self.torrents.inspect(&task.url) => i.map_err(state)?,
                    () = cancel.cancelled() => return Err(shardload_engine::Error::Cancelled.into()),
                };
                let chosen: Vec<&TorrentFile> = match &choice.files {
                    Some(ids) => ids.iter().filter_map(|&i| info.files.get(i)).collect(),
                    None => info.files.iter().collect(),
                };
                let size: u64 = chosen.iter().map(|f| f.size).sum();
                std::fs::create_dir_all(&task.dir).map_err(shardload_engine::Error::from)?;
                let _one_at_a_time = self.naming.lock().await;
                let taken: std::collections::HashSet<PathBuf> =
                    self.db.paths_except(task.id).await?.into_iter().collect();
                let free = |p: &Path| !p.exists() && !taken.contains(p);
                let base = shardload_engine::name::sanitize(&info.name);
                let (folder, path, name) = match info.files.as_slice() {
                    // One file: straight into the folder unless the name is taken there.
                    [only] if free(&task.dir.join(&only.name)) => (
                        task.dir.clone(),
                        task.dir.join(&only.name),
                        only.name.clone(),
                    ),
                    _ => {
                        let folder = shardload_engine::target_avoiding(
                            &task.dir,
                            &base,
                            &task.url,
                            conflict,
                            |p| taken.contains(p),
                        );
                        let name = folder
                            .file_name()
                            .map_or_else(|| base.clone(), |n| n.to_string_lossy().into_owned());
                        let path = match info.files.as_slice() {
                            [only] => folder.join(&only.name),
                            _ => folder.clone(),
                        };
                        (folder, path, name)
                    }
                };
                let category = match chosen.as_slice() {
                    [one] => Category::for_name(&one.name),
                    _ => Category::for_name(&name),
                };
                self.db
                    .set_target(task.id, &name, &path, Some(size), category)
                    .await?;
                choice.folder = Some(folder.clone());
                stream.torrent = Some(choice.clone());
                self.db.set_stream(task.id, &stream).await?;
                task.name = Some(name);
                task.path = Some(path);
                task.size = Some(size);
                task.category = category;
                task.stream = Some(stream);
                self.emit(Event::Updated(task.clone()));
                folder
            }
        };
        self.torrents.set_limit(self.global.rate());
        let (tx, rx) = watch::channel(Progress::default());
        let reporter = tokio::spawn(report(
            task.id,
            rx,
            self.events.clone(),
            self.db.clone(),
            None,
            hints::Watch::default(),
        ));
        let result = self
            .torrents
            .download(&task.url, &folder, choice.files, limit, &tx, cancel)
            .await;
        drop(tx);
        reporter.await.ok();
        match result {
            Ok(done) => Ok(Outcome {
                size: done.size,
                resumed_from: 0,
                elapsed: started.elapsed(),
                checksum: None,
                checksum_from: None,
                structure: None,
            }),
            Err(_) if cancel.is_cancelled() => Err(shardload_engine::Error::Cancelled.into()),
            Err(e) => Err(state(e)),
        }
    }

    /// A page of a site yt-dlp knows: yt-dlp downloads the chosen format; the file's real
    /// extension is known only at the end. `mark` gives the file the Mark of the Web, with
    /// the page as its address.
    pub(crate) async fn download_page(
        &self,
        mut task: Task,
        format: &str,
        tools: &ytdlp::Tools,
        conflict: Conflict,
        mark: bool,
        cancel: &CancellationToken,
    ) -> Result<Outcome> {
        let started = std::time::Instant::now();
        let path = match &task.path {
            Some(p) => p.clone(),
            None => {
                std::fs::create_dir_all(&task.dir).map_err(shardload_engine::Error::from)?;
                let stem = task.name.clone().unwrap_or_else(|| "video".to_owned());
                let stem = match &task.stream {
                    Some(s) => clip_stem(shardload_engine::name::split_extension(&stem).0, s),
                    None => shardload_engine::name::split_extension(&stem).0.to_owned(),
                };
                let name = shardload_engine::name::sanitize(&format!("{stem}.mp4"));
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
        let stem = path.with_extension("");
        let (tx, rx) = watch::channel(Progress::default());
        let reporter = tokio::spawn(report(
            task.id,
            rx,
            self.events.clone(),
            self.db.clone(),
            None,
            hints::Watch::default(),
        ));
        let clip = task.stream.as_ref().and_then(|s| s.clip);
        let record = task.stream.as_ref().and_then(|s| s.record.clone());
        // A recording ends at its limit like the user stopping it.
        let stop = cancel.child_token();
        if let Some(limit) = record.as_ref().and_then(|r| r.limit) {
            let stop = stop.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(limit)).await;
                stop.cancel();
            });
        }
        let result = ytdlp::run(
            tools,
            &task.url,
            format,
            clip.as_ref(),
            record.as_ref(),
            &stem,
            &task.headers,
            &tx,
            &stop,
        )
        .await;
        stop.cancel();
        drop(tx);
        reporter.await.ok();
        let mut file = match result {
            Ok(f) => f,
            Err(_) if cancel.is_cancelled() => {
                return Err(shardload_engine::Error::Cancelled.into());
            }
            Err(e) => return Err(shardload_engine::Error::State(e).into()),
        };
        // A recording is written as MPEG-TS, whatever its name: an MP4 with ffmpeg.
        if record.is_some()
            && let Some(ffmpeg) = &tools.ffmpeg
        {
            match ytdlp::remux(ffmpeg, &file, &file.with_extension("mp4")).await {
                Ok(mp4) => file = mp4,
                Err(e) => tracing::warn!(id = task.id, "recording kept as MPEG-TS: {e}"),
            }
        }
        if file != path {
            // Not merged into MP4 (audio alone, or a single file of another type).
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let category = Category::for_name(&name);
            self.db
                .set_target(task.id, &name, &file, None, category)
                .await?;
        }
        let size = std::fs::metadata(&file)
            .map_err(shardload_engine::Error::from)?
            .len();
        if mark {
            shardload_engine::mark_of_the_web(&file, &task.url, &task.headers);
        }
        Ok(Outcome {
            size,
            resumed_from: 0,
            elapsed: started.elapsed(),
            checksum: None,
            checksum_from: None,
            structure: None,
        })
    }
}
