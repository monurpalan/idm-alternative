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
    /// Folder per category, used instead of `dir` for new downloads of that category (IDM
    /// keeps e.g. `Downloads\Video`).
    pub category_dirs: HashMap<Category, PathBuf>,
    /// Downloads running at the same time; the rest wait in the queue.
    pub max_active: usize,
    /// Connections per download unless the download says otherwise.
    pub connections: usize,
    /// What happens when the server's file name is taken in the folder.
    pub conflict: Conflict,
    /// Client settings every download starts from (browser profile, proxy, timeouts…).
    pub engine: Options,
    /// A download that fails for a reason that may pass (network, server busy or erroring)
    /// is retried this many times before it stays failed.
    pub max_retries: u32,
    /// Wait before the first retry; doubles with each one, up to five minutes.
    pub retry_delay: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("."),
            category_dirs: HashMap::new(),
            max_active: 3,
            connections: 16,
            conflict: Conflict::Rename,
            engine: Options::default(),
            max_retries: 5,
            retry_delay: Duration::from_secs(5),
        }
    }
}

/// Serialized as `{"type": "added", "data": {...}}` for the UI.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", content = "data", rename_all = "lowercase")]
pub enum Event {
    Added(Task),
    /// Status, name, size or settings changed.
    Updated(Task),
    Progress(TaskProgress),
    Removed(TaskId),
}

/// Where to move a download in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Move {
    Top,
    Up,
    Down,
    Bottom,
}

/// Longest wait between automatic retries.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(300);

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
    /// Downloads waiting to be retried after a failure that may pass.
    retries: Mutex<HashMap<TaskId, Retry>>,
    closed: AtomicBool,
}

struct Retry {
    /// Failures in a row so far.
    attempts: u32,
    /// Not started again before this.
    at: Instant,
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

/// Failures that may pass by themselves: the network, or a server that is busy or erroring.
/// Not: a missing file (4xx), a changed file, a full disk, an expired link or a checksum
/// mismatch, which need the user.
fn retryable(e: &Error) -> bool {
    use idma_engine::Error as E;
    match e {
        Error::Engine(e) => match e {
            E::Http(_)
            | E::Busy(_)
            | E::Truncated
            | E::Stalled(_)
            | E::TooManyFailures { .. }
            | E::BadRange(_) => true,
            E::Status(code) => *code >= 500 || *code == 408 || *code == 429,
            _ => false,
        },
        _ => false,
    }
}

/// The file and the engine's partial data and state next to it.
fn delete_files_of(path: &Path) {
    for suffix in ["", ".idma-part", ".idma-state"] {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        std::fs::remove_file(PathBuf::from(p)).ok();
    }
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
            retries: Mutex::new(HashMap::new()),
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

    /// A value saved with [`Manager::save_setting`], or `None` if there is none (or it no
    /// longer parses as `T`).
    pub async fn setting<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        Ok(self
            .inner
            .db
            .setting(key)
            .await?
            .and_then(|v| serde_json::from_str(&v).ok()))
    }

    pub async fn save_setting<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.inner
            .db
            .save_setting(key, &serde_json::to_string(value)?)
            .await
    }

    /// Reads a setting before opening the manager, e.g. to build its [`Config`].
    pub async fn read_setting<T: serde::de::DeserializeOwned>(
        db_path: &Path,
        key: &str,
    ) -> Result<Option<T>> {
        let db = Db::open(db_path).await?;
        let value = db.setting(key).await;
        db.close().await;
        Ok(value?.and_then(|v| serde_json::from_str(&v).ok()))
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
        let name = new.name.as_deref().map(idma_engine::name::sanitize);
        // A guess from the URL until the server names the file.
        let category = Category::for_name(
            &name
                .clone()
                .or_else(|| idma_engine::name::from_url(&new.url))
                .unwrap_or_default(),
        );
        let (dir, connections) = {
            let cfg = lock(&self.inner.config);
            (
                new.dir.clone().unwrap_or_else(|| {
                    cfg.category_dirs.get(&category).unwrap_or(&cfg.dir).clone()
                }),
                new.connections.unwrap_or(cfg.connections),
            )
        };
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
                category,
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
        // Started by hand: no waiting for a scheduled retry.
        lock(&self.inner.retries).remove(&id);
        if matches!(task.status, Status::Paused | Status::Error) {
            self.inner.set_status(id, Status::Queued, None).await?;
        }
        self.inner.fill().await
    }

    /// Stops a running download (keeping its progress) or takes a queued one out of the
    /// queue. Returns once the download has stopped.
    pub async fn pause(&self, id: TaskId) -> Result<()> {
        let task = self.get(id).await?;
        lock(&self.inner.retries).remove(&id);
        if !self.inner.stop(id, Stop::Pause).await && task.status == Status::Queued {
            self.inner.set_status(id, Status::Paused, None).await?;
        }
        Ok(())
    }

    /// Removes a download from the list, stopping it first. With `delete_files`, the file
    /// and any partial data go too; otherwise files stay where they are.
    pub async fn remove(&self, id: TaskId, delete_files: bool) -> Result<()> {
        let task = self.get(id).await?;
        lock(&self.inner.retries).remove(&id);
        self.inner.stop(id, Stop::Remove).await;
        self.inner.db.delete(id).await?;
        if delete_files && let Some(path) = &task.path {
            delete_files_of(path);
        }
        self.inner.emit(Event::Removed(id));
        self.inner.fill().await
    }

    /// Downloads again from scratch (IDM's "Redownload"): the file and any partial data are
    /// deleted and the download is queued.
    pub async fn restart(&self, id: TaskId) -> Result<()> {
        let task = self.get(id).await?;
        lock(&self.inner.retries).remove(&id);
        // Stopped like a removal: the download's own end is not recorded.
        self.inner.stop(id, Stop::Remove).await;
        if let Some(path) = &task.path {
            delete_files_of(path);
        }
        self.inner.db.reset(id).await?;
        let task = self.get(id).await?;
        self.inner.emit(Event::Updated(task));
        self.inner.fill().await
    }

    /// Asks the server about `url` with the configured client settings, without adding
    /// anything: for showing name and size before the user decides.
    pub async fn probe(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<idma_engine::Probe> {
        let opts = lock(&self.inner.config).engine.clone();
        Ok(idma_engine::probe(url, headers, &opts).await?)
    }

    /// Queues every paused or failed download.
    pub async fn start_all(&self) -> Result<()> {
        for t in self.list().await? {
            if matches!(t.status, Status::Paused | Status::Error) {
                lock(&self.inner.retries).remove(&t.id);
                self.inner.set_status(t.id, Status::Queued, None).await?;
            }
        }
        self.inner.fill().await
    }

    /// Pauses every running and queued download.
    pub async fn pause_all(&self) -> Result<()> {
        // Queued ones first, so stopping a running one does not start the next.
        let tasks = self.list().await?;
        for status in [Status::Queued, Status::Downloading] {
            for t in tasks.iter().filter(|t| t.status == status) {
                self.pause(t.id).await?;
            }
        }
        Ok(())
    }

    /// Moves a download in the queue order, among the downloads that are not finished.
    pub async fn move_task(&self, id: TaskId, to: Move) -> Result<()> {
        let tasks = self.list().await?; // in queue order
        let order: Vec<&Task> = tasks.iter().filter(|t| t.status != Status::Done).collect();
        let Some(i) = order.iter().position(|t| t.id == id) else {
            return Ok(());
        };
        let mut changed = vec![];
        match to {
            Move::Top => {
                let first = tasks.first().map_or(0, |t| t.queue_pos);
                self.inner.db.set_queue_pos(id, first - 1).await?;
                changed.push(id);
            }
            Move::Bottom => {
                let last = tasks.last().map_or(0, |t| t.queue_pos);
                self.inner.db.set_queue_pos(id, last + 1).await?;
                changed.push(id);
            }
            Move::Up | Move::Down => {
                let j = if to == Move::Up {
                    i.checked_sub(1)
                } else {
                    Some(i + 1).filter(|j| *j < order.len())
                };
                let Some(j) = j else { return Ok(()) };
                let (a, b) = (order[i], order[j]);
                self.inner.db.set_queue_pos(a.id, b.queue_pos).await?;
                self.inner.db.set_queue_pos(b.id, a.queue_pos).await?;
                changed.extend([a.id, b.id]);
            }
        }
        for id in changed {
            self.inner.emit(Event::Updated(self.get(id).await?));
        }
        Ok(())
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

    /// Points a download at a fresh link for the same file (the old one expired, or the site
    /// hands out a new one per visit), keeping its progress. The new link must serve the same
    /// file: the same size, and the same bytes as already downloaded; otherwise this fails
    /// with [`idma_engine::Error::FileChanged`] and nothing changes. A download that was
    /// running or had failed continues with the new link.
    pub async fn refresh_link(&self, id: TaskId, url: &str) -> Result<()> {
        let task = self.get(id).await?;
        let was_running = self.inner.stop(id, Stop::Pause).await;
        if let Some(path) = &task.path {
            let mut state = path.as_os_str().to_owned();
            state.push(".idma-state");
            if PathBuf::from(state).exists() {
                let opts = lock(&self.inner.config).engine.clone();
                let req = idma_engine::Request {
                    url: url.to_owned(),
                    output: path.clone(),
                    headers: task.headers.clone(),
                };
                if let Err(e) = idma_engine::patch_url(&req, &opts).await {
                    if was_running {
                        self.start(id).await?;
                    }
                    return Err(e.into());
                }
            }
        }
        self.inner.db.set_url(id, url).await?;
        self.inner.emit(Event::Updated(self.get(id).await?));
        if was_running || task.status == Status::Error {
            self.start(id).await?;
        }
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
        let waiting: Vec<TaskId> = {
            let now = Instant::now();
            lock(&self.retries)
                .iter()
                .filter(|(_, r)| r.at > now)
                .map(|(id, _)| *id)
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
            (Ok(outcome), _) => {
                lock(&self.retries).remove(&id);
                self.db.finish(id, outcome.size, now_ms()).await
            }
            (Err(Error::Engine(idma_engine::Error::Cancelled)), Some(Stop::Pause)) => {
                self.db.set_status(id, Status::Paused, None).await
            }
            // Stays "downloading" so the next open resumes it.
            (Err(Error::Engine(idma_engine::Error::Cancelled)), Some(Stop::Shutdown)) => Ok(()),
            (Err(Error::Engine(idma_engine::Error::Cancelled)), Some(Stop::Remove)) => Ok(()),
            (Err(e), _) => match self.schedule_retry(id, e) {
                Some(delay) => {
                    tracing::info!(id, "download failed, retrying in {delay:?}: {e}");
                    let note = format!("{e} (retrying in {} s)", delay.as_secs().max(1));
                    self.db.set_status(id, Status::Queued, Some(&note)).await
                }
                None => {
                    tracing::warn!(id, "download failed: {e}");
                    self.db
                        .set_status(id, Status::Error, Some(&e.to_string()))
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
    }

    /// After a failure: when it may pass and retries are left, records when to try again,
    /// wakes the queue then, and returns the wait.
    fn schedule_retry(self: &Arc<Self>, id: TaskId, e: &Error) -> Option<Duration> {
        let (max, base) = {
            let cfg = lock(&self.config);
            (cfg.max_retries, cfg.retry_delay)
        };
        let mut retries = lock(&self.retries);
        let attempts = retries.get(&id).map_or(0, |r| r.attempts) + 1;
        if !retryable(e) || attempts > max {
            retries.remove(&id);
            return None;
        }
        let delay = base
            .saturating_mul(1 << (attempts - 1).min(16))
            .min(MAX_RETRY_DELAY);
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
        Some(delay)
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
