//! A plain download: the site's memory, the server asked first, the engine, retries,
//! ffmpeg's setup, and its progress for the UI.

use super::*;

impl Inner {
    /// After a failure: when it may pass and retries are left, records when to try again,
    /// wakes the queue then, and returns the wait and whether the machine is offline.
    pub(crate) fn schedule_retry(
        self: &Arc<Self>,
        id: TaskId,
        e: &Error,
    ) -> Option<(Duration, bool)> {
        let (max, base) = {
            let cfg = lock(&self.config);
            (cfg.max_retries, cfg.retry_delay)
        };
        let mut retries = lock(&self.retries);
        let previous = retries.get(&id).map_or(0, |r| r.attempts);
        let offline = matches!(e, Error::Engine(engine) if retry::offline(engine));
        let Some((attempts, delay)) = retry::plan(previous, max, base, retryable(e), offline)
        else {
            retries.remove(&id);
            return None;
        };
        retries.insert(
            id,
            Retry {
                attempts,
                at: Instant::now() + delay,
            },
        );
        let inner = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if let Err(e) = inner.fill().await {
                tracing::error!("could not start queued downloads: {e}");
            }
        });
        Some((delay, offline))
    }

    /// ffmpeg from the app's [`FfmpegSetup`], with its progress shown as the download's.
    /// The inner error says why there is none.
    pub(crate) async fn set_up_ffmpeg(
        &self,
        id: TaskId,
        cancel: &CancellationToken,
    ) -> Result<std::result::Result<PathBuf, String>> {
        let (setup, opts) = {
            let cfg = lock(&self.config);
            (cfg.ffmpeg_setup.clone(), cfg.engine.clone())
        };
        let Some(FfmpegSetup(setup)) = setup else {
            return Ok(Err("this program cannot set it up".into()));
        };
        let (tx, mut rx) = watch::channel(Progress::default());
        let events = self.events.clone();
        let forward = tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                let p = rx.borrow_and_update().clone();
                let _ = events.send(Event::Progress(TaskProgress {
                    id,
                    phase: shardload_engine::Phase::SettingUp,
                    size: p.size,
                    received: p.received,
                    speed: p.speed,
                    connections: p.connections,
                    verified: 0,
                    checksum_from: None,
                    recorded: 0.0,
                    eta: None,
                    hints: Vec::new(),
                    segments: Vec::new(),
                }));
                tokio::time::sleep(PROGRESS_EVERY).await;
            }
        });
        // Shown at once, before the first byte.
        tx.send_modify(|_| {});
        let result = setup(opts, tx, cancel.clone()).await;
        forward.await.ok();
        match result {
            Ok(path) => {
                // For the downloads that come next.
                lock(&self.config).ffmpeg = Some(path.clone());
                Ok(Ok(path))
            }
            Err(_) if cancel.is_cancelled() => Err(shardload_engine::Error::Cancelled.into()),
            Err(e) => {
                tracing::warn!(id, "could not set up ffmpeg: {e}");
                Ok(Err(e))
            }
        }
    }

    /// Asks about the task's file; when the site refuses (403) the identity the settings
    /// give and has not said which it serves, the others are tried, and the one it serves is
    /// remembered and used for the download.
    pub(crate) async fn probe_as_allowed(
        &self,
        task: &Task,
        opts: &mut Options,
        site: Option<&str>,
        memory: &mut Option<SiteMemory>,
    ) -> Result<shardload_engine::Probe> {
        let first = match probe_any(task, opts).await {
            Ok(p) => return Ok(p),
            Err(e) => e,
        };
        let refused = matches!(
            first,
            shardload_engine::Error::LinkExpired(403) | shardload_engine::Error::Status(403)
        );
        let Some(site) = site.filter(|_| refused) else {
            return Err(first.into());
        };
        if memory.as_ref().is_some_and(|m| m.browser.is_some()) {
            return Err(first.into());
        }
        let refused_as = opts.browser;
        for identity in sites::IDENTITIES.into_iter().filter(|b| *b != refused_as) {
            let mut other = opts.clone();
            other.browser = identity;
            if let Ok(p) = probe_any(task, &other).await {
                let name = sites::browser_name(identity);
                tracing::info!(
                    site,
                    "refused as {}, served as {name}",
                    sites::browser_name(refused_as)
                );
                opts.browser = identity;
                let m = memory.get_or_insert_with(|| SiteMemory {
                    site: site.to_owned(),
                    ..SiteMemory::default()
                });
                m.browser = Some(name.to_owned());
                return Ok(p);
            }
        }
        Err(first.into())
    }

    /// Keeps `note` with the download (see [`Inner::notes`]) and shows it now.
    pub(crate) async fn note(&self, task: &mut Task, note: String) -> Result<()> {
        self.db
            .set_status(task.id, Status::Downloading, Some(&note))
            .await?;
        task.error = Some(note.clone());
        lock(&self.notes).insert(task.id, note);
        self.emit(Event::Updated(task.clone()));
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        clippy::cognitive_complexity,
        reason = "one long sequence kept together; to be split when it next changes"
    )]
    pub(crate) async fn download(
        &self,
        mut task: Task,
        cancel: &CancellationToken,
        limiter: Limiter,
    ) -> Result<Outcome> {
        let (mut opts, conflict, ffmpeg, ytdlp, deno) = {
            let cfg = lock(&self.config);
            (
                cfg.engine.clone(),
                cfg.conflict,
                cfg.ffmpeg.clone(),
                cfg.ytdlp.clone(),
                cfg.deno.clone(),
            )
        };
        opts.connections = task.connections;
        opts.limiters = vec![self.global.clone(), limiter];
        opts.checksum = task
            .checksum
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(Error::BadChecksum)?;
        opts.checksum_from = task.checksum_from.clone();
        opts.page_checksum = task.page_checksum.as_deref().and_then(|c| c.parse().ok());
        opts.pieces = task.pieces.clone();

        let page_format = task.stream.as_ref().and_then(|s| s.ytdlp.clone());
        if page_format.is_some() && ytdlp.is_none() {
            return Err(shardload_engine::Error::State(
                "yt-dlp is not set up (Settings, Video tools)".into(),
            )
            .into());
        }
        // Set up the first time a download needs to join video and audio.
        let mut no_ffmpeg = None;
        let ffmpeg = match ffmpeg {
            None if task.stream.as_ref().is_some_and(joins_tracks) => {
                match self.set_up_ffmpeg(task.id, cancel).await? {
                    Ok(path) => Some(path),
                    Err(why) => {
                        no_ffmpeg = Some(why);
                        None
                    }
                }
            }
            other => other,
        };

        if let Some(mut format) = page_format {
            let ytdlp = ytdlp.expect("checked above");
            let fallback = no_ffmpeg.clone().filter(|_| ytdlp::merges(&format));
            if let (Some(why), Some(_)) = (&no_ffmpeg, task.stream.as_ref().and_then(|s| s.clip)) {
                return Err(shardload_engine::Error::State(format!(
                    "cutting a clip needs ffmpeg, which could not be set up: {why}"
                ))
                .into());
            }
            if let Some(why) = &fallback {
                // Never two files (a silent video and its sound): one format with both.
                format = ytdlp::single_file(&format);
                let note = format!(
                    "Saved as one file with its own sound, which may be lower quality than \
                     chosen: ffmpeg, which joins separate video and audio, is not set up \
                     ({why}). Download it again once it is (Settings, Video tools)."
                );
                self.note(&mut task, note).await?;
            }
            let tools = ytdlp::Tools {
                ytdlp,
                deno,
                ffmpeg,
            };
            let result = self
                .download_page(
                    task,
                    &format,
                    &tools,
                    conflict,
                    opts.mark_of_the_web,
                    cancel,
                )
                .await;
            return match (result, fallback) {
                // No format with both (YouTube has none any more): nothing was downloaded.
                (Err(Error::Engine(shardload_engine::Error::State(e))), Some(why))
                    if e.contains("Requested format is not available") =>
                {
                    Err(shardload_engine::Error::State(format!(
                        "this video only comes as separate video and audio, and ffmpeg, which \
                         joins them, could not be set up ({why}); start the download again to \
                         try once more"
                    ))
                    .into())
                }
                (result, _) => result,
            };
        }
        if torrent::is_torrent(&task.url) {
            let limit = task.speed_limit;
            return self.download_torrent(task, conflict, limit, cancel).await;
        }
        if let Some(mut stream) = task.stream.clone() {
            match stream.archive.clone() {
                Some(choice) if !choice.whole => {
                    match self
                        .download_archive(
                            task.clone(),
                            stream.clone(),
                            choice,
                            &opts,
                            conflict,
                            cancel,
                        )
                        .await
                    {
                        // The whole archive instead, from now on.
                        Err(Error::Engine(shardload_engine::Error::NoRanges)) => {
                            tracing::info!(
                                id = task.id,
                                "no range requests: downloading the whole archive"
                            );
                            if let Some(a) = stream.archive.as_mut() {
                                a.whole = true;
                            }
                            self.db.set_stream(task.id, &stream).await?;
                            task.stream = Some(stream);
                        }
                        other => return other,
                    }
                }
                Some(_) => {}
                None => {
                    if let Some(why) = no_ffmpeg.filter(|_| stream.audio.is_some()) {
                        return Err(shardload_engine::Error::State(format!(
                            "this stream has separate audio, which ffmpeg joins with the \
                             video, and ffmpeg could not be set up: {why}"
                        ))
                        .into());
                    }
                    return self
                        .download_stream(task, stream, opts, conflict, ffmpeg, cancel)
                        .await;
                }
            }
        }

        // What earlier downloads learned about the site: connections, ranges, identity.
        let site = site_of(&task.url);
        let mut memory = match &site {
            Some(s) => sites::recall(&self.db, s, now_ms()).await?,
            None => None,
        };
        if let Some(m) = &memory {
            m.apply(&mut opts);
        }
        opts.learn = shardload_engine::Learning::default();
        let tried = opts.connections;
        let started_before = task.path.is_some();

        let path = match &task.path {
            Some(p) => p.clone(),
            None => {
                // First start: ask the server for the name and size, then pick a free path.
                let probe = tokio::select! {
                    p = self.probe_as_allowed(&task, &mut opts, site.as_deref(), &mut memory) => p?,
                    () = cancel.cancelled() => return Err(shardload_engine::Error::Cancelled.into()),
                };
                std::fs::create_dir_all(&task.dir).map_err(shardload_engine::Error::from)?;
                let name = task.name.clone().unwrap_or(probe.name);
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
                let category = Category::for_name(&file_name);
                self.db
                    .set_target(task.id, &file_name, &path, probe.size, category)
                    .await?;
                if probe.url != task.url {
                    self.db.set_final_url(task.id, &probe.url).await?;
                    task.final_url = Some(probe.url);
                }
                task.name = Some(file_name);
                task.path = Some(path.clone());
                task.size = probe.size;
                task.category = category;
                self.emit(Event::Updated(task.clone()));
                path
            }
        };

        let req = shardload_engine::Request {
            url: task.url.clone(),
            output: path,
            headers: task.headers.clone(),
            mirrors: task.mirrors.clone(),
        };
        let (tx, rx) = watch::channel(Progress::default());
        let prior = memory.as_ref().and_then(|m| m.speed).map(|s| s as f64);
        let watch = hints::Watch {
            global: opts.limiters.first().cloned(),
            own: opts.limiters.get(1).cloned(),
            proxy: opts.proxy.is_some(),
        };
        let reporter = tokio::spawn(report(
            task.id,
            rx,
            self.events.clone(),
            self.db.clone(),
            prior,
            watch,
        ));
        let result = shardload_engine::download(&req, &opts, Some(&tx), cancel).await;
        drop(tx);
        reporter.await.ok();
        if let Some(site) = site {
            let learned = opts.learn.get();
            let known = memory.is_some();
            let mut m = memory.unwrap_or(SiteMemory {
                site,
                ..SiteMemory::default()
            });
            m.learn(&learned, tried, task.connections, result.is_ok());
            if let Ok(o) = &result {
                m.moved(o.size - o.resumed_from, o.elapsed.as_secs_f64());
            }
            // A link that worked and then stopped: how long it lasted.
            if matches!(result, Err(shardload_engine::Error::LinkExpired(_)))
                && (started_before || learned.peak_connections > 0)
            {
                m.expired_after(((now_ms() - task.link_at).max(0) / 1000) as u64);
            }
            if known || !m.is_empty() {
                sites::keep(&self.db, m, now_ms()).await?;
            }
        }
        // A checksum found for the file stays with the download, so the list shows it and
        // the next attempt checks against the same one.
        let found = match &result {
            Ok(Outcome {
                checksum: Some(c),
                checksum_from: Some(from),
                ..
            }) => Some((c.to_string(), from)),
            Err(shardload_engine::Error::ChecksumMismatch {
                expected,
                origin: Some(from),
                ..
            }) => Some((expected.clone(), from)),
            _ => None,
        };
        if task.checksum.is_none()
            && let Some((checksum, from)) = found
        {
            self.db.set_found_checksum(task.id, &checksum, from).await?;
        }
        Ok(result?)
    }
}

/// Forwards a running download's progress as events, and saves it now and then.
pub(crate) async fn report(
    id: TaskId,
    mut rx: watch::Receiver<Progress>,
    events: broadcast::Sender<Event>,
    db: Db,
    prior: Option<f64>,
    watch: hints::Watch,
) {
    let mut tick = tokio::time::interval(PROGRESS_EVERY);
    let mut saved = Instant::now();
    let started = Instant::now();
    let mut eta = eta::Estimator::new(prior);
    let mut hints = hints::Hints::new(watch);
    loop {
        tick.tick().await;
        match rx.has_changed() {
            Err(_) => {
                // The download ended: keep its last count for the list.
                let p = rx.borrow().clone();
                if p.received > 0
                    && let Err(e) = db.set_received(id, p.received, p.size).await
                {
                    tracing::warn!(id, "could not save progress: {e}");
                }
                return;
            }
            Ok(false) => continue,
            Ok(true) => {}
        }
        let p = rx.borrow_and_update().clone();
        let left = match (p.phase, p.size) {
            (Phase::Downloading, Some(size)) => {
                eta.update(started.elapsed().as_secs_f64(), p.received);
                eta.eta(size.saturating_sub(p.received))
            }
            _ => None,
        };
        if saved.elapsed() >= SAVE_EVERY {
            saved = Instant::now();
            if let Err(e) = db.set_received(id, p.received, p.size).await {
                tracing::warn!(id, "could not save progress: {e}");
            }
        }
        let hints = hints.update(&p.health, p.phase == Phase::Downloading, Instant::now());
        let _ = events.send(Event::Progress(TaskProgress {
            id,
            phase: p.phase,
            size: p.size,
            received: p.received,
            speed: p.speed,
            connections: p.connections,
            verified: p.verified,
            checksum_from: p.checksum_from,
            recorded: p.recorded,
            eta: left.map(|(soonest, latest)| [soonest, latest]),
            hints,
            segments: p.segments,
        }));
    }
}
