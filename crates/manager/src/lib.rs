//! The download list on top of the engine: tasks stored in SQLite, a queue that runs a few
//! at a time, pause/resume/remove, speed limits, and a stream of events for the UI.
//!
//! The desktop app is a thin shell over [`Manager`]; the native host (browser integration)
//! will add downloads through the same API.
//!
//! - Live progress goes out as [`Event::Progress`] about four times a second per running
//!   download; the database is written at most about once a second per download (ADR 0001).
//! - Closing the manager (or the process dying) leaves running downloads marked as
//!   downloading; the next [`Manager::open`] queues them again and they resume from their
//!   state files.

mod db;
mod task;

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use idma_engine::{CancellationToken, Checksum, Conflict, Limiter, Options, Outcome, Progress};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use db::{Db, Insert};
pub use task::{Category, NewTask, Status, Task, TaskId, TaskProgress};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    #[error("database migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("stored data: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Engine(#[from] idma_engine::Error),
    #[error("no download with id {0}")]
    NotFound(TaskId),
    #[error("bad checksum: {0}")]
    BadChecksum(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone)]
pub struct Config {
    /// Default folder for new downloads.
    pub dir: PathBuf,
    /// Downloads running at the same time; the rest wait in the queue.
    pub max_active: usize,
    /// Connections per download unless the download says otherwise.
    pub connections: usize,
    /// What happens when the server's file name is taken in the folder.
    pub conflict: Conflict,
    /// Client settings every download starts from (browser profile, proxy, timeouts…).
    pub engine: Options,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("."),
            max_active: 3,
            connections: 16,
            conflict: Conflict::Rename,
            engine: Options::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Added(Task),
    /// Status, name, size or settings changed.
    Updated(Task),
    Progress(TaskProgress),
    Removed(TaskId),
}

/// How often running downloads report progress.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// How often a running download's `received` is written to the database.
const SAVE_EVERY: Duration = Duration::from_secs(1);

/// Handle to the download list; cheap to clone.
#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    events: broadcast::Sender<Event>,
    /// Shared by every download.
    global: Limiter,
    config: Mutex<Config>,
    running: Mutex<HashMap<TaskId, Running>>,
    /// Held while picking queued downloads to start, so two callers never start the same one.
    scheduling: tokio::sync::Mutex<()>,
    /// Held while choosing a download's path, so two downloads never get the same one.
    naming: tokio::sync::Mutex<()>,
    closed: AtomicBool,
}

struct Running {
    cancel: CancellationToken,
    /// Why it was cancelled; read by the download's task when it stops.
    stop: Arc<Mutex<Option<Stop>>>,
    limiter: Limiter,
    handle: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Pause,
    Remove,
    /// The manager is closing; the download resumes on the next open.
    Shutdown,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl Manager {
    /// Opens (or creates) the list at `db_path`. Downloads that were running when the list
    /// was last closed are queued again, and queued downloads start.
    pub async fn open(db_path: &Path, config: Config) -> Result<Self> {
        let db = Db::open(db_path).await?;
        let requeued = db.requeue_interrupted().await?;
        if requeued > 0 {
            tracing::info!(requeued, "resuming downloads interrupted by the last close");
        }
        let inner = Arc::new(Inner {
            db,
            events: broadcast::channel(1024).0,
            global: Limiter::new(None),
            config: Mutex::new(config),
            running: Mutex::new(HashMap::new()),
            scheduling: tokio::sync::Mutex::new(()),
            naming: tokio::sync::Mutex::new(()),
            closed: AtomicBool::new(false),
        });
        inner.fill().await?;
        Ok(Self { inner })
    }

    /// Events for every change from now on. A receiver that falls far behind skips events
    /// (`RecvError::Lagged`) and should reload the list.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub async fn list(&self) -> Result<Vec<Task>> {
        self.inner.db.list().await
    }

    pub async fn get(&self, id: TaskId) -> Result<Task> {
        self.inner.db.get(id).await?.ok_or(Error::NotFound(id))
    }

    pub fn config(&self) -> Config {
        lock(&self.inner.config).clone()
    }

    /// Applies to downloads started from now on; a larger `max_active` starts queued ones.
    pub async fn set_config(&self, config: Config) -> Result<()> {
        *lock(&self.inner.config) = config;
        self.inner.fill().await
    }

    pub async fn add(&self, new: NewTask) -> Result<Task> {
        if let Some(c) = &new.checksum {
            c.parse::<Checksum>().map_err(Error::BadChecksum)?;
        }
        let (dir, connections) = {
            let cfg = lock(&self.inner.config);
            (
                new.dir.clone().unwrap_or_else(|| cfg.dir.clone()),
                new.connections.unwrap_or(cfg.connections),
            )
        };
        let name = new.name.as_deref().map(idma_engine::name::sanitize);
        let id = self
            .inner
            .db
            .insert(Insert {
                url: &new.url,
                headers: &new.headers,
                dir: &dir,
                name: name.as_deref(),
                status: if new.start {
                    Status::Queued
                } else {
                    Status::Paused
                },
                category: name
                    .as_deref()
                    .map_or(Category::General, Category::for_name),
                connections: connections.max(1),
                speed_limit: new.speed_limit,
                checksum: new.checksum.as_deref(),
                added_at: now_ms(),
            })
            .await?;
        let task = self.get(id).await?;
        self.inner.emit(Event::Added(task.clone()));
        if new.start {
            self.inner.fill().await?;
        }
        Ok(task)
    }

    /// Queues a paused, failed or queued download; it starts when a slot is free.
    pub async fn start(&self, id: TaskId) -> Result<()> {
        let task = self.get(id).await?;
        if matches!(task.status, Status::Paused | Status::Error) {
            self.inner.set_status(id, Status::Queued, None).await?;
        }
        self.inner.fill().await
    }

    /// Stops a running download (keeping its progress) or takes a queued one out of the
    /// queue. Returns once the download has stopped.
    pub async fn pause(&self, id: TaskId) -> Result<()> {
        let task = self.get(id).await?;
        if !self.inner.stop(id, Stop::Pause).await && task.status == Status::Queued {
            self.inner.set_status(id, Status::Paused, None).await?;
        }
        Ok(())
    }

    /// Removes a download from the list, stopping it first. With `delete_files`, the file
    /// and any partial data go too; otherwise files stay where they are.
    pub async fn remove(&self, id: TaskId, delete_files: bool) -> Result<()> {
        let task = self.get(id).await?;
        self.inner.stop(id, Stop::Remove).await;
        self.inner.db.delete(id).await?;
        if delete_files && let Some(path) = &task.path {
            for suffix in ["", ".idma-part", ".idma-state"] {
                let mut p = path.as_os_str().to_owned();
                p.push(suffix);
                std::fs::remove_file(PathBuf::from(p)).ok();
            }
        }
        self.inner.emit(Event::Removed(id));
        self.inner.fill().await
    }

    /// Limit for all downloads together, bytes per second; takes effect at once.
    pub fn set_global_limit(&self, rate: Option<u64>) {
        self.inner.global.set_rate(rate);
    }

    pub fn global_limit(&self) -> Option<u64> {
        self.inner.global.rate()
    }

    /// One download's own limit, bytes per second; takes effect at once if it is running.
    pub async fn set_speed_limit(&self, id: TaskId, rate: Option<u64>) -> Result<()> {
        self.inner.db.set_speed_limit(id, rate).await?;
        if let Some(r) = lock(&self.inner.running).get(&id) {
            r.limiter.set_rate(rate);
        }
        let task = self.get(id).await?;
        self.inner.emit(Event::Updated(task));
        Ok(())
    }

    /// Stops every running download so it resumes on the next [`Manager::open`], and closes
    /// the database.
    pub async fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        let ids: Vec<TaskId> = lock(&self.inner.running).keys().copied().collect();
        for id in ids {
            self.inner.stop(id, Stop::Shutdown).await;
        }
        self.inner.db.close().await;
    }
}

impl Inner {
    fn emit(&self, e: Event) {
        // No receivers is fine.
        let _ = self.events.send(e);
    }

    async fn set_status(&self, id: TaskId, status: Status, error: Option<&str>) -> Result<()> {
        self.db.set_status(id, status, error).await?;
        if let Some(task) = self.db.get(id).await? {
            self.emit(Event::Updated(task));
        }
        Ok(())
    }

    /// Cancels a running download for `why` and waits until it has stopped. `false` if it was
    /// not running.
    async fn stop(&self, id: TaskId, why: Stop) -> bool {
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
    fn fill(self: &Arc<Self>) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(self.fill_now())
    }

    async fn fill_now(self: &Arc<Self>) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let _one_at_a_time = self.scheduling.lock().await;
        let max = lock(&self.config).max_active.max(1);
        let free = max.saturating_sub(lock(&self.running).len());
        if free == 0 {
            return Ok(());
        }
        for mut task in self.db.queued(free).await? {
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

    /// One download from start to end, then records how it ended and starts the next.
    async fn run(
        self: Arc<Self>,
        task: Task,
        cancel: CancellationToken,
        stop: Arc<Mutex<Option<Stop>>>,
        limiter: Limiter,
    ) {
        let id = task.id;
        let result = self.download(task, &cancel, limiter).await;
        let why = *lock(&stop);
        let recorded = match (&result, why) {
            (Ok(outcome), _) => self.db.finish(id, outcome.size, now_ms()).await,
            (Err(Error::Engine(idma_engine::Error::Cancelled)), Some(Stop::Pause)) => {
                self.db.set_status(id, Status::Paused, None).await
            }
            // Stays "downloading" so the next open resumes it.
            (Err(Error::Engine(idma_engine::Error::Cancelled)), Some(Stop::Shutdown)) => Ok(()),
            (Err(Error::Engine(idma_engine::Error::Cancelled)), Some(Stop::Remove)) => Ok(()),
            (Err(e), _) => {
                tracing::warn!(id, "download failed: {e}");
                self.db
                    .set_status(id, Status::Error, Some(&e.to_string()))
                    .await
            }
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
    }

    async fn download(
        &self,
        mut task: Task,
        cancel: &CancellationToken,
        limiter: Limiter,
    ) -> Result<Outcome> {
        let (mut opts, conflict) = {
            let cfg = lock(&self.config);
            (cfg.engine.clone(), cfg.conflict)
        };
        opts.connections = task.connections;
        opts.limiters = vec![self.global.clone(), limiter];
        opts.checksum = task
            .checksum
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(Error::BadChecksum)?;

        let path = match &task.path {
            Some(p) => p.clone(),
            None => {
                // First start: ask the server for the name and size, then pick a free path.
                let probe = tokio::select! {
                    p = idma_engine::probe(&task.url, &task.headers, &opts) => p?,
                    () = cancel.cancelled() => return Err(idma_engine::Error::Cancelled.into()),
                };
                std::fs::create_dir_all(&task.dir).map_err(idma_engine::Error::from)?;
                let name = task.name.clone().unwrap_or(probe.name);
                let _one_at_a_time = self.naming.lock().await;
                let taken: std::collections::HashSet<PathBuf> =
                    self.db.paths_except(task.id).await?.into_iter().collect();
                let path =
                    idma_engine::target_avoiding(&task.dir, &name, &task.url, conflict, |p| {
                        taken.contains(p)
                    });
                let file_name = path
                    .file_name()
                    .map_or_else(|| name.clone(), |n| n.to_string_lossy().into_owned());
                let category = Category::for_name(&file_name);
                self.db
                    .set_target(task.id, &file_name, &path, probe.size, category)
                    .await?;
                task.name = Some(file_name);
                task.path = Some(path.clone());
                task.size = probe.size;
                task.category = category;
                self.emit(Event::Updated(task.clone()));
                path
            }
        };

        let req = idma_engine::Request {
            url: task.url.clone(),
            output: path,
            headers: task.headers.clone(),
        };
        let (tx, rx) = watch::channel(Progress::default());
        let reporter = tokio::spawn(report(task.id, rx, self.events.clone(), self.db.clone()));
        let result = idma_engine::download(&req, &opts, Some(&tx), cancel).await;
        drop(tx);
        reporter.await.ok();
        Ok(result?)
    }
}

/// Forwards a running download's progress as events, and saves it now and then.
async fn report(
    id: TaskId,
    mut rx: watch::Receiver<Progress>,
    events: broadcast::Sender<Event>,
    db: Db,
) {
    let mut tick = tokio::time::interval(PROGRESS_EVERY);
    let mut saved = Instant::now();
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
        if saved.elapsed() >= SAVE_EVERY {
            saved = Instant::now();
            if let Err(e) = db.set_received(id, p.received, p.size).await {
                tracing::warn!(id, "could not save progress: {e}");
            }
        }
        let _ = events.send(Event::Progress(TaskProgress {
            id,
            phase: p.phase,
            size: p.size,
            received: p.received,
            speed: p.speed,
            connections: p.connections,
            verified: p.verified,
            segments: p.segments,
        }));
    }
}
