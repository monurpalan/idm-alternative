//! The queue: starting downloads in free slots, the disk watch, and how a download's
//! end is recorded (with what the finished file turned out to be).

use super::*;

impl Inner {
    pub(crate) fn free_space(&self) -> disk::FreeSpace {
        lock(&self.config).free_space.as_ref().map_or_else(
            || Arc::new(disk::free_space) as disk::FreeSpace,
            |s| s.0.clone(),
        )
    }

    /// Pauses the downloads on a drive that has come within [`disk::RESERVE`] of full, and
    /// the ones waiting to start there, before writing fails half way.
    pub(crate) async fn watch_disks(self: &Arc<Self>) -> Result<()> {
        if lock(&self.running).is_empty() {
            return Ok(());
        }
        let tasks: Vec<Task> = self
            .db
            .list()
            .await?
            .into_iter()
            .filter(|t| matches!(t.status, Status::Downloading | Status::Queued))
            .collect();
        for drive in disk::needs(&tasks, &self.free_space()) {
            let Some(free) = drive.free.filter(|f| *f < disk::RESERVE) else {
                continue;
            };
            let running = lock(&self.running).keys().copied().collect::<HashSet<_>>();
            if !drive.tasks.iter().any(|id| running.contains(id)) {
                continue;
            }
            let note = format!(
                "paused: only {} MB left on {}; free some space or move it to another drive, \
                 then resume",
                free >> 20,
                drive.volume.display()
            );
            tracing::warn!("{note}");
            // The waiting ones first: a running one stopped first frees its slot, and the
            // queue would start one of them in it.
            let mut order = drive.tasks.clone();
            order.sort_by_key(|id| running.contains(id));
            for &id in &order {
                lock(&self.retries).remove(&id);
                self.stop(id, Stop::Pause).await;
                self.db
                    .set_failure(id, Status::Paused, Some(&note), Some(ErrorKind::Disk))
                    .await?;
                if let Some(t) = self.db.get(id).await? {
                    self.emit(Event::Updated(t));
                }
            }
            self.emit(Event::DiskLow(disk::DiskLow {
                volume: drive.volume,
                free,
                paused: drive.tasks,
            }));
        }
        Ok(())
    }

    pub(crate) fn emit(&self, e: Event) {
        // No receivers is fine.
        let _ = self.events.send(e);
    }

    /// A recording set to start later is held back until then, when the queue is filled.
    pub(crate) fn start_later(self: &Arc<Self>, task: &Task) {
        let Some(at) = task
            .stream
            .as_ref()
            .and_then(|s| s.record.as_ref())
            .and_then(|r| r.start_at)
            .filter(|at| *at > now_ms())
        else {
            return;
        };
        lock(&self.later).insert(task.id, at);
        let inner = Arc::clone(self);
        let wait = Duration::from_millis((at - now_ms()).max(0) as u64);
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            if let Err(e) = inner.fill().await {
                tracing::error!("could not start queued downloads: {e}");
            }
        });
    }

    pub(crate) async fn set_status(
        &self,
        id: TaskId,
        status: Status,
        error: Option<&str>,
    ) -> Result<()> {
        self.db.set_status(id, status, error).await?;
        if let Some(task) = self.db.get(id).await? {
            self.emit(Event::Updated(task));
        }
        Ok(())
    }

    /// Cancels a running download for `why` and waits until it has stopped. `false` if it was
    /// not running.
    pub(crate) async fn stop(&self, id: TaskId, why: Stop) -> bool {
        let handle = {
            let mut running = lock(&self.running);
            let Some(r) = running.get_mut(&id) else {
                return false;
            };
            *lock(&r.stop) = Some(why);
            r.cancel.cancel();
            r.handle.take()
        };
        if let Some(h) = handle {
            h.await.ok();
        }
        true
    }

    /// Starts queued downloads while there are free slots.
    ///
    /// Boxed: a download calls this again when it ends, and without a boxed `Send` future in
    /// the cycle the compiler cannot prove the nested futures are `Send`.
    pub(crate) fn fill(self: &Arc<Self>) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(self.fill_now())
    }

    pub(crate) async fn fill_now(self: &Arc<Self>) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let _one_at_a_time = self.scheduling.lock().await;
        let max = lock(&self.config).max_active.max(1);
        let free = max.saturating_sub(lock(&self.running).len());
        if free == 0 {
            return Ok(());
        }
        let waiting: Vec<TaskId> = {
            let now = Instant::now();
            let mut later = lock(&self.later);
            later.retain(|_, at| *at > now_ms());
            lock(&self.retries)
                .iter()
                .filter(|(_, r)| r.at > now)
                .map(|(id, _)| *id)
                .chain(lock(&self.moving).iter().copied())
                .chain(later.keys().copied())
                .collect()
        };
        let queued = self.db.queued(free + waiting.len()).await?;
        let startable = queued.into_iter().filter(|t| !waiting.contains(&t.id));
        for mut task in startable.take(free) {
            self.db
                .set_status(task.id, Status::Downloading, None)
                .await?;
            task.status = Status::Downloading;
            task.error = None;
            self.emit(Event::Updated(task.clone()));

            let cancel = CancellationToken::new();
            let stop = Arc::new(Mutex::new(None));
            let limiter = Limiter::new(task.speed_limit);
            let id = task.id;
            // Holding the map while spawning: the download removes its own entry when it ends,
            // which must not happen before the entry exists.
            let mut running = lock(&self.running);
            let handle = tokio::spawn(Arc::clone(self).run(
                task,
                cancel.clone(),
                Arc::clone(&stop),
                limiter.clone(),
            ));
            running.insert(
                id,
                Running {
                    cancel,
                    stop,
                    limiter,
                    handle: Some(handle),
                },
            );
        }
        Ok(())
    }

    /// Puts the login saved for the task's site into its headers, unless that is the login
    /// the server just turned down. Whether it did.
    pub(crate) async fn use_saved_login(&self, id: TaskId) -> bool {
        let Ok(Some(task)) = self.db.get(id).await else {
            return false;
        };
        let Some(site) = site_of(&task.url) else {
            return false;
        };
        let Ok(Some((user, password))) = self.db.login(&site).await else {
            return false;
        };
        let auth = shardload_engine::basic_auth(&user, &password);
        if task.headers.iter().any(|h| h == &auth) {
            return false;
        }
        let mut headers: Vec<(String, String)> = task
            .headers
            .into_iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("authorization"))
            .collect();
        headers.push(auth);
        self.db.set_headers(id, &headers).await.is_ok()
    }

    /// One download from start to end, then records how it ended and starts the next.
    #[allow(
        clippy::too_many_lines,
        clippy::cognitive_complexity,
        reason = "one long sequence kept together; to be split when it next changes"
    )]
    pub(crate) async fn run(
        self: Arc<Self>,
        task: Task,
        cancel: CancellationToken,
        stop: Arc<Mutex<Option<Stop>>>,
        limiter: Limiter,
    ) {
        let id = task.id;
        let result = self.download(task, &cancel, limiter).await;
        let note = lock(&self.notes).remove(&id);
        let why = *lock(&stop);
        // The server asks to log in and a login is saved for the site: try it once.
        let logged_in = why.is_none()
            && matches!(
                result,
                Err(Error::Engine(shardload_engine::Error::AuthRequired { .. }))
            )
            && self.use_saved_login(id).await;
        let recorded = match (&result, why) {
            _ if logged_in => {
                tracing::info!(id, "trying the saved login");
                self.db.set_status(id, Status::Queued, None).await
            }
            (Ok(outcome), _) => 'done: {
                lock(&self.retries).remove(&id);
                match self.inspect(id).await {
                    Ok(None) => {}
                    Ok(Some(page)) => {
                        tracing::warn!(id, "the server sent a web page instead of the file");
                        break 'done self
                            .db
                            .fail_from_start(id, &page, Some(ErrorKind::Expired))
                            .await;
                    }
                    Err(e) => tracing::warn!(id, "could not check what the file is: {e}"),
                }
                let structure = outcome.structure.as_ref().map(crate::task::Structure::from);
                if let Err(e) = self.db.set_structure(id, structure.as_ref()).await {
                    tracing::warn!(id, "could not keep how its structure checked out: {e}");
                }
                let at = now_ms();
                // In the history before it shows as done: whatever sees it done finds it there.
                if let Ok(Some(mut task)) = self.db.get(id).await {
                    task.size = Some(outcome.size);
                    task.finished_at = Some(at);
                    let page = task
                        .headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("referer"))
                        .map(|(_, v)| v.as_str());
                    if let Err(e) = self.db.record_history(&task, page).await {
                        tracing::warn!(id, "could not keep the download in the history: {e}");
                    }
                    self.hash_later(&task);
                }
                match self.db.finish(id, outcome.size, at).await {
                    Ok(()) if note.is_some() => {
                        self.db.set_status(id, Status::Done, note.as_deref()).await
                    }
                    other => other,
                }
            }
            (Err(Error::Engine(shardload_engine::Error::Cancelled)), Some(Stop::Pause)) => {
                self.db.set_status(id, Status::Paused, None).await
            }
            // Stays "downloading" so the next open resumes it.
            (Err(Error::Engine(shardload_engine::Error::Cancelled)), Some(Stop::Shutdown)) => {
                Ok(())
            }
            (Err(Error::Engine(shardload_engine::Error::Cancelled)), Some(Stop::Remove)) => Ok(()),
            (Err(e), _) => match self.schedule_retry(id, e) {
                Some((delay, true)) => {
                    tracing::info!(id, "no network connection, checking again in {delay:?}");
                    let note = format!(
                        "no network connection (checking again in {} s)",
                        delay.as_secs().max(1)
                    );
                    self.db
                        .set_failure(id, Status::Queued, Some(&note), Some(ErrorKind::Offline))
                        .await
                }
                Some((delay, false)) => {
                    tracing::info!(id, "download failed, retrying in {delay:?}: {e}");
                    let note = format!("{e} (retrying in {} s)", delay.as_secs().max(1));
                    self.db.set_status(id, Status::Queued, Some(&note)).await
                }
                None => {
                    tracing::warn!(id, "download failed: {e}");
                    let kind = match e {
                        Error::Engine(e) => ErrorKind::of(e),
                        _ => None,
                    };
                    self.db
                        .set_failure(id, Status::Error, Some(&e.to_string()), kind)
                        .await
                }
            },
        };
        if let Err(e) = recorded {
            tracing::error!(id, "could not record the end of a download: {e}");
        }
        if why != Some(Stop::Remove)
            && let Ok(Some(task)) = self.db.get(id).await
        {
            self.emit(Event::Updated(task));
        }
        lock(&self.running).remove(&id);
        if let Err(e) = self.fill().await {
            tracing::error!("could not start queued downloads: {e}");
        }
        if why.is_none() && result.is_ok() {
            let inner = Arc::clone(&self);
            tokio::spawn(async move { inner.after_done(id).await });
        }
    }

    /// Reads the start of a finished file and keeps what it is. A name that says nothing
    /// (`download.php`), chosen from the server's answer, gets the extension of what the file
    /// is (and the page's title when its stem says nothing either). A web page where the name
    /// promised a file is deleted and its message returned: the link led to a sign-in page or
    /// an error, not to the file.
    pub(crate) async fn inspect(&self, id: TaskId) -> Result<Option<String>> {
        use shardload_engine::sniff;
        let Some(task) = self.db.get(id).await? else {
            return Ok(None);
        };
        // Files this reads: a plain download's, not a stream's or a torrent's.
        let (Some(path), Some(name), None) = (&task.path, &task.name, &task.stream) else {
            return Ok(None);
        };
        if torrent::is_torrent(&task.url) || !path.is_file() {
            return Ok(None);
        }
        let head = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || sniff::read_head(&path))
                .await
                .map_err(|e| shardload_engine::Error::from(std::io::Error::other(e)))?
                .map_err(shardload_engine::Error::from)?
        };
        let content = sniff::sniff(&head);
        self.db
            .set_content(id, content.map(sniff::Content::label).as_deref())
            .await?;
        if sniff::check(name, content) == Some(sniff::Finding::WebPage) {
            delete_files_of(path);
            return Ok(Some(format!(
                "the server sent a web page, not {name}: often a sign-in page or a link that \
                 expired; get a fresh link from the site"
            )));
        }
        if task.name_given {
            return Ok(None);
        }
        let Some(better) = sniff::better_name(name, content, task.page_title.as_deref()) else {
            return Ok(None);
        };
        let _one_at_a_time = self.naming.lock().await;
        let taken: HashSet<PathBuf> = self.db.paths_except(id).await?.into_iter().collect();
        let to = shardload_engine::target_avoiding(&task.dir, &better, "", Conflict::Rename, |p| {
            taken.contains(p)
        });
        if let Err(e) = std::fs::rename(path, &to) {
            tracing::warn!(
                id,
                "could not rename {} to {}: {e}",
                path.display(),
                to.display()
            );
            return Ok(None);
        }
        let new_name = file_name(&to);
        tracing::info!(id, "named {name} by its content: {new_name}");
        self.db
            .set_place(
                id,
                &task.dir,
                Some(&new_name),
                Some(&to),
                Category::for_name(&new_name),
            )
            .await?;
        Ok(None)
    }
}
