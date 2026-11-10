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
        let (handle, own, _seen_it_end) = {
            let mut running = lock(&self.running);
            let Some(r) = running.get_mut(&id) else {
                return false;
            };
            {
                // A removal stays one: a removed download's end is not recorded.
                let mut reason = lock(&r.stop);
                if *reason != Some(Stop::Remove) {
                    *reason = Some(why);
                }
            }
            r.cancel.cancel();
            // A stop that comes meanwhile waits for this one to see it end.
            let (seen, wait) = tokio::sync::oneshot::channel::<()>();
            let next = tokio::spawn(async move {
                wait.await.ok();
            });
            (r.handle.replace(next), Arc::clone(&r.stop), seen)
        };
        if let Some(h) = handle {
            h.await.ok();
        }
        // Its run is over and no other stop waits for it (only the entry and this hold its
        // reason): a run that panicked never took its entry out.
        let mut running = lock(&self.running);
        if running
            .get(&id)
            .is_some_and(|r| Arc::ptr_eq(&r.stop, &own) && Arc::strong_count(&own) == 2)
        {
            running.remove(&id);
        }
        true
    }

    /// Stops the download for `why` if it runs, and keeps the queue from starting anything
    /// while the guard is held: what is done to it next is not raced by a start. Whether it
    /// was running.
    pub(crate) async fn stop_held(
        &self,
        id: TaskId,
        why: Stop,
    ) -> (bool, tokio::sync::MutexGuard<'_, ()>) {
        let mut stopped = false;
        loop {
            // A fill that is starting it has it running once this is held.
            let guard = self.scheduling.lock().await;
            if !lock(&self.running).contains_key(&id) {
                return (stopped, guard);
            }
            drop(guard);
            stopped |= self.stop(id, why).await;
        }
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
        // Closed while this waited: `close` has stopped what runs, and nothing starts now.
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        // A run that panicked is gone without taking its entry out: only the entry holds its
        // reason then, while a run or a stop waiting for it holds it too.
        lock(&self.running).retain(|_, r| Arc::strong_count(&r.stop) > 1);
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
                // Queued again by its own run before that run has ended (a fresh link, a
                // saved login): it starts once that run is gone.
                .chain(lock(&self.running).keys().copied())
                .collect()
        };
        // Some may be held by the network rules: enough are read to skip them, and each
        // says why it waits.
        let queued = self.db.queued(free + waiting.len() + 256).await?;
        let mut startable = Vec::new();
        // The earliest deadline first, then the rest in the queue's order.
        let deadlines = lock(&self.deadlines).at.clone();
        let mut queued = queued;
        queued.sort_by_key(|t| {
            t.deadline
                .or(deadlines.get(&t.id).copied())
                .unwrap_or(i64::MAX)
        });
        // A recording set to start later waits for its time, also before `start_later` has
        // noted it (a fill can come between adding it and that).
        let later = |t: &Task| {
            t.stream
                .as_ref()
                .and_then(|s| s.record.as_ref())
                .and_then(|r| r.start_at)
                .is_some_and(|at| at > now_ms())
        };
        for t in queued
            .into_iter()
            .filter(|t| !waiting.contains(&t.id) && !later(t))
        {
            match self.held(&t) {
                None => startable.push(t),
                Some(why) if t.error.as_deref() != Some(why) => {
                    self.db.set_status(t.id, Status::Queued, Some(why)).await?;
                    if let Some(t) = self.db.get(t.id).await? {
                        self.emit(Event::Updated(t));
                    }
                }
                Some(_) => {}
            }
        }
        for mut task in startable.into_iter().take(free) {
            self.db
                .set_status(task.id, Status::Downloading, None)
                .await?;
            task.status = Status::Downloading;
            task.error = None;
            self.emit(Event::Updated(task.clone()));
            #[cfg(test)]
            tests::starting(&task.url).await;

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
    pub(crate) async fn run(
        self: Arc<Self>,
        task: Task,
        cancel: CancellationToken,
        stop: Arc<Mutex<Option<Stop>>>,
        limiter: Limiter,
    ) {
        let id = task.id;
        self.peek(&task, &cancel);
        let result = self.download(task, &cancel, limiter).await;
        let note = lock(&self.notes).remove(&id);
        let why = *lock(&stop);
        // The server asks to log in and a login is saved for the site: try it once.
        let logged_in = why.is_none() && self.saved_login_may_help(id, &result).await;
        let recorded = match (&result, why) {
            _ if logged_in => {
                tracing::info!(id, "trying the saved login");
                self.db.set_status(id, Status::Queued, None).await
            }
            // A recording stopped ends well; removed (or downloaded again), it is not done.
            (Ok(_), Some(Stop::Remove)) => Ok(()),
            (Ok(outcome), _) => self.record_done(id, outcome, note).await,
            (Err(Error::Engine(shardload_engine::Error::Cancelled)), Some(Stop::Pause)) => {
                self.db.set_status(id, Status::Paused, None).await
            }
            // Stays "downloading" so the next open resumes it.
            (Err(Error::Engine(shardload_engine::Error::Cancelled)), Some(Stop::Shutdown)) => {
                Ok(())
            }
            (Err(Error::Engine(shardload_engine::Error::Cancelled)), Some(Stop::Remove)) => Ok(()),
            (Err(e), _) => self.record_failure(id, e).await,
        };
        if let Err(e) = recorded {
            tracing::error!(id, "could not record the end of a download: {e}");
        }
        self.ended(id, &stop, why, result.is_ok()).await;
    }

    /// The server asked to log in (or for an SSH key's passphrase) and a login is saved for
    /// the site: the download is set to use it.
    async fn saved_login_may_help(&self, id: TaskId, result: &Result<Outcome>) -> bool {
        matches!(
            result,
            Err(Error::Engine(
                shardload_engine::Error::AuthRequired { .. }
                    | shardload_engine::Error::KeyLocked { .. }
            ))
        ) && self.use_saved_login(id).await
    }

    /// After a download's end is recorded: the list told, the next queued downloads started,
    /// and (when it finished) what comes after it.
    async fn ended(
        self: &Arc<Self>,
        id: TaskId,
        stop: &Arc<Mutex<Option<Stop>>>,
        why: Option<Stop>,
        finished: bool,
    ) {
        if why != Some(Stop::Remove)
            && let Ok(Some(task)) = self.db.get(id).await
        {
            self.emit(Event::Updated(task));
        }
        {
            // Only its own entry: never one of a later run of the same download.
            let mut running = lock(&self.running);
            if running.get(&id).is_some_and(|r| Arc::ptr_eq(&r.stop, stop)) {
                running.remove(&id);
            }
        }
        if let Err(e) = self.fill().await {
            tracing::error!("could not start queued downloads: {e}");
        }
        if why.is_none() && finished {
            let inner = Arc::clone(self);
            tokio::spawn(async move { inner.after_done(id).await });
        }
    }

    /// Records a download that finished: what the file is, its structure, the history.
    async fn record_done(
        self: &Arc<Self>,
        id: TaskId,
        outcome: &Outcome,
        note: Option<String>,
    ) -> Result<()> {
        lock(&self.retries).remove(&id);
        if let Some(page) = self.web_page_instead(id).await {
            return self
                .db
                .fail_from_start(id, &page, Some(ErrorKind::Expired))
                .await;
        }
        let structure = outcome.structure.as_ref().map(crate::task::Structure::from);
        if let Err(e) = self.db.set_structure(id, structure.as_ref()).await {
            tracing::warn!(id, "could not keep how its structure checked out: {e}");
        }
        let at = now_ms();
        // In the history before it shows as done: whatever sees it done finds it there.
        self.keep_in_history(id, outcome.size, at).await;
        match self.db.finish(id, outcome.size, at).await {
            Ok(()) if note.is_some() => self.db.set_status(id, Status::Done, note.as_deref()).await,
            other => other,
        }
    }

    /// The message of the web page a finished download turned out to be, instead of the file.
    async fn web_page_instead(&self, id: TaskId) -> Option<String> {
        match self.inspect(id).await {
            Ok(None) => None,
            Ok(Some(page)) => {
                tracing::warn!(id, "the server sent a web page instead of the file");
                Some(page)
            }
            Err(e) => {
                tracing::warn!(id, "could not check what the file is: {e}");
                None
            }
        }
    }

    /// Keeps a finished download of `size` bytes in the history, and its hash for later.
    async fn keep_in_history(self: &Arc<Self>, id: TaskId, size: u64, at: i64) {
        if let Ok(Some(mut task)) = self.db.get(id).await {
            task.size = Some(size);
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
    }

    /// Records a download that failed: queued again to retry, or failed for good.
    async fn record_failure(self: &Arc<Self>, id: TaskId, e: &Error) -> Result<()> {
        match self.schedule_retry(id, e) {
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
            // An expired link is first looked for again on its page, so the download is
            // never shown as failed while a fresh link carries it on.
            None if matches!(e, Error::Engine(e) if ErrorKind::of(e) == Some(ErrorKind::Expired))
                && self.refresh_from_page(id).await =>
            {
                Ok(())
            }
            None => self.failed(id, e).await,
        }
    }

    /// Records a download that failed for good.
    async fn failed(&self, id: TaskId, e: &Error) -> Result<()> {
        tracing::warn!(id, "download failed: {e}");
        let kind = match e {
            Error::Engine(e) => ErrorKind::of(e),
            _ => None,
        };
        self.db
            .set_failure(id, Status::Error, Some(&e.to_string()), kind)
            .await
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future as _;
    use std::task::Poll;
    use tokio::sync::Notify;

    /// Downloads held between being marked as started and being running: their URL, a
    /// notice that one got there, and the go-ahead.
    static HELD: Mutex<Vec<Held>> = Mutex::new(Vec::new());

    type Held = (String, Arc<Notify>, Arc<Notify>);

    /// Waits for the go-ahead when the download of `url` is held there.
    pub(super) async fn starting(url: &str) {
        let held = {
            let mut held = lock(&HELD);
            let i = held.iter().position(|(u, ..)| u == url);
            i.map(|i| held.swap_remove(i))
        };
        if let Some((_, reached, go)) = held {
            reached.notify_one();
            go.notified().await;
        }
    }

    /// A download, queued and held as it starts. Its manager and id, the notice that it is
    /// held, and the go-ahead.
    async fn held_start(name: &str) -> (Manager, TaskId, Arc<Notify>, Arc<Notify>) {
        let (m, id, url) = queued(name).await;
        let reached = Arc::new(Notify::new());
        let go = Arc::new(Notify::new());
        lock(&HELD).push((url, Arc::clone(&reached), Arc::clone(&go)));
        (m, id, reached, go)
    }

    /// A slow download, queued but not started: its manager, id and URL.
    async fn queued(name: &str) -> (Manager, TaskId, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let app = shardload_testserver::router();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
        });
        let dir = std::env::temp_dir()
            .join("shardload-manager-unit")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("dir");
        let config = Config {
            dir: dir.join("downloads"),
            max_active: 2,
            connections: 1,
            engine: shardload_engine::Options {
                find_checksum: false,
                ..shardload_engine::Options::default()
            },
            ..Config::default()
        };
        let m = Manager::open(&dir.join("list.db"), config)
            .await
            .expect("open");
        let url = format!("{base}/file/{name}.bin?size=8M&seed=7&rate=1M");
        let task = m
            .add(NewTask {
                url: url.clone(),
                start: false,
                ..NewTask::default()
            })
            .await
            .expect("add");
        m.inner
            .db
            .set_status(task.id, Status::Queued, None)
            .await
            .expect("queue");
        (m, task.id, url)
    }

    /// Leaves an entry in the running downloads as a download that panicked would: its task
    /// over, not cancelled, never taken out.
    async fn leak_entry(m: &Manager, id: TaskId) {
        let handle = tokio::spawn(async {});
        while !handle.is_finished() {
            tokio::task::yield_now().await;
        }
        lock(&m.inner.running).insert(
            id,
            Running {
                cancel: CancellationToken::new(),
                stop: Arc::new(Mutex::new(None)),
                limiter: Limiter::new(None),
                handle: Some(handle),
            },
        );
    }

    /// Waits until the download is running for real (not the entry left behind).
    async fn wait_started(m: &Manager, id: TaskId) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let started = lock(&m.inner.running)
                    .get(&id)
                    .and_then(|r| r.handle.as_ref())
                    .is_some_and(|h| !h.is_finished());
                if started && m.get(id).await.expect("get").received > 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("it never started");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_download_whose_run_was_lost_starts_again() {
        let (m, id, _) = queued("leaked-start").await;
        leak_entry(&m, id).await;
        m.inner.fill().await.expect("fill");
        wait_started(&m, id).await;
        m.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_download_whose_run_was_lost_can_be_paused_and_removed() {
        let (m, id, _) = queued("leaked-stop").await;
        leak_entry(&m, id).await;
        tokio::time::timeout(Duration::from_secs(10), m.pause(id))
            .await
            .expect("the pause hangs")
            .expect("pause");
        assert_eq!(m.get(id).await.expect("get").status, Status::Paused);
        assert!(!lock(&m.inner.running).contains_key(&id));
        leak_entry(&m, id).await;
        tokio::time::timeout(Duration::from_secs(10), m.remove(id, true))
            .await
            .expect("the removal hangs")
            .expect("remove");
        assert!(m.get(id).await.is_err());
        m.close().await;
    }

    /// Starts the held download, does `what` while it is held, lets it go, and waits for
    /// both. `what` is given a while to finish before the download goes on: done right, it
    /// waits for the start instead.
    async fn while_starting<F>(m: &Manager, reached: &Notify, go: &Notify, what: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let inner = Arc::clone(&m.inner);
        let fill = tokio::spawn(async move { inner.fill().await });
        reached.notified().await;
        let mut what = tokio::spawn(what);
        let done_early = tokio::select! {
            biased;
            r = &mut what => { r.expect("join"); true }
            () = tokio::time::sleep(Duration::from_millis(500)) => false,
        };
        go.notify_one();
        fill.await.expect("join").expect("fill");
        if !done_early {
            what.await.expect("join");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pause_as_a_download_starts_stops_it() {
        let (m, id, reached, go) = held_start("start-pause").await;
        let pausing = m.clone();
        while_starting(&m, &reached, &go, async move {
            pausing.pause(id).await.expect("pause");
        })
        .await;
        assert_eq!(m.get(id).await.expect("get").status, Status::Paused);
        assert!(!lock(&m.inner.running).contains_key(&id));
        m.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_removal_as_a_download_starts_stops_it() {
        let (m, id, reached, go) = held_start("start-remove").await;
        let removing = m.clone();
        while_starting(&m, &reached, &go, async move {
            removing.remove(id, true).await.expect("remove");
        })
        .await;
        assert!(m.get(id).await.is_err());
        assert!(!lock(&m.inner.running).contains_key(&id));
        m.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn closing_as_a_download_starts_stops_it() {
        let (m, id, reached, go) = held_start("start-close").await;
        let closing = m.clone();
        while_starting(&m, &reached, &go, async move {
            closing.close().await;
        })
        .await;
        assert!(!lock(&m.inner.running).contains_key(&id));
    }

    /// A second stop of the same download waits for it too, and a removal is not turned
    /// into a pause by a stop that comes after it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_second_stop_waits_and_keeps_a_removal() {
        let (m, id, _reached, go) = held_start("stop-twice").await;
        go.notify_one();
        m.inner.fill().await.expect("fill");
        while m.get(id).await.expect("get").received == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let inner = Arc::clone(&m.inner);
        let mut removing = Box::pin(inner.stop(id, Stop::Remove));
        let first = std::future::poll_fn(|cx| Poll::Ready(removing.as_mut().poll(cx))).await;
        assert!(first.is_pending());
        let pausing = async {
            assert!(inner.stop(id, Stop::Pause).await);
            lock(&inner.running).contains_key(&id)
        };
        let (removed, still_running) = tokio::join!(removing, pausing);
        assert!(removed);
        assert!(!still_running, "the second stop returned before it stopped");
        // Not recorded as paused: a removed download's end is not recorded.
        assert_eq!(m.get(id).await.expect("get").status, Status::Downloading);
        m.close().await;
    }
}
