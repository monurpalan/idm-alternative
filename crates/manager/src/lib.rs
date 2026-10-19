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

pub mod batch;
mod db;
pub mod folder;
pub mod grabber;
mod relocate;
mod retry;
mod secret;
mod task;
pub mod torrent;
pub mod ytdlp;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use shardload_engine::{
    CancellationToken, Checksum, Conflict, Limiter, Options, Outcome, Progress,
};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use db::{Db, Insert};
pub use task::{
    ArchiveChoice, Category, ErrorKind, NewTask, Status, Stream, Subtitle, Task, TaskId,
    TaskProgress, TorrentChoice,
};
pub use torrent::{TorrentConfig, TorrentFile, TorrentInfo};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    #[error("database migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("stored data: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Engine(#[from] shardload_engine::Error),
    #[error("no download with id {0}")]
    NotFound(TaskId),
    #[error("bad checksum: {0}")]
    BadChecksum(String),
    /// A rename or move that cannot be done, and why.
    #[error("{0}")]
    CannotMove(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Sets up ffmpeg when a download needs it (to join separate video and audio) and the
/// configuration has none: gets the client settings, reports the setup's progress on the
/// sender, stops when cancelled, and returns the program, which the manager then keeps in
/// its configuration.
#[derive(Clone)]
pub struct FfmpegSetup(pub Arc<SetupFn>);

type SetupFn =
    dyn Fn(Options, watch::Sender<Progress>, CancellationToken) -> SetupFuture + Send + Sync;
type SetupFuture = Pin<Box<dyn Future<Output = std::result::Result<PathBuf, String>> + Send>>;

impl std::fmt::Debug for FfmpegSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FfmpegSetup")
    }
}

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
    /// is retried this many times before it stays failed. While the machine has no route to
    /// the internet, a download waits instead, and these retries do not count.
    pub max_retries: u32,
    /// Wait before the first retry; doubles with each one, up to five minutes.
    pub retry_delay: Duration,
    /// Puts stream video and audio into one file; without it a stream is saved as the
    /// joined `.ts` segments, and one with separate audio cannot be saved.
    pub ffmpeg: Option<PathBuf>,
    /// Sets ffmpeg up while it is missing: without it (or when it fails), a page is saved as
    /// a single file, which may be lower quality, and a stream with separate audio cannot be
    /// saved.
    pub ffmpeg_setup: Option<FfmpegSetup>,
    /// yt-dlp, for pages of sites such as YouTube; and deno, which it needs there.
    pub ytdlp: Option<PathBuf>,
    pub deno: Option<PathBuf>,
    /// Torrents and magnet links; read when the first one starts.
    pub torrent: TorrentConfig,
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
            ffmpeg: None,
            ffmpeg_setup: None,
            ytdlp: None,
            deno: None,
            torrent: TorrentConfig::default(),
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
    /// A download's files are being copied to another drive (see [`Manager::relocate`]);
    /// an [`Event::Updated`] follows when they are in place, or the move failed.
    Moving(MoveProgress),
}

/// How far the copy of a download's files has come.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct MoveProgress {
    pub id: TaskId,
    pub copied: u64,
    pub total: u64,
}

/// What a ZIP archive on a server holds (see [`Manager::inspect_archive`]).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArchiveInfo {
    /// The archive's file name.
    pub name: String,
    pub size: u64,
    pub zip64: bool,
    pub entries: Vec<ArchiveEntry>,
}

/// One file or folder in an archive.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArchiveEntry {
    /// Path inside the archive, `/`-separated; folders end with `/`.
    pub name: String,
    pub size: u64,
    pub compressed_size: u64,
    /// `stored`, `deflate`, `bzip2`, `lzma`...
    pub method: String,
    pub is_dir: bool,
    pub encrypted: bool,
    /// Why it cannot be extracted (encrypted, or a compression method other than stored and
    /// deflate).
    pub unsupported: Option<String>,
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
    /// Downloads whose files are being renamed or moved; the queue does not start them.
    moving: Mutex<HashSet<TaskId>>,
    torrents: torrent::Torrents,
    closed: AtomicBool,
    /// Said about a download that went differently than asked (saved as a single file
    /// without ffmpeg); kept with it once it is done.
    notes: Mutex<HashMap<TaskId, String>>,
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

/// `film.mp4` → `film.<label>.<ext>`, the name players look for next to a video.
fn subtitle_path(video: &Path, label: &str, ext: &str) -> PathBuf {
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    video.with_file_name(shardload_engine::name::sanitize(&format!(
        "{stem}.{label}.{ext}"
    )))
}

/// Whether a download joins separate video and audio, which needs ffmpeg: a page in a
/// format that merges two, or a stream with its own audio.
fn joins_tracks(stream: &Stream) -> bool {
    match &stream.ytdlp {
        Some(format) => ytdlp::merges(format),
        None => stream.audio.is_some() && stream.torrent.is_none() && stream.archive.is_none(),
    }
}

/// Failures that may pass by themselves: the network, or a server that is busy or erroring.
/// Not: a missing file (4xx), a changed file, a full disk, an expired link or a checksum
/// mismatch, which need the user.
fn retryable(e: &Error) -> bool {
    use shardload_engine::Error as E;
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

/// `old` with every header named in `fresh` replaced by the fresh value.
fn merge_headers(old: &[(String, String)], fresh: &[(String, String)]) -> Vec<(String, String)> {
    old.iter()
        .filter(|(k, _)| !fresh.iter().any(|(f, _)| f.eq_ignore_ascii_case(k)))
        .chain(fresh)
        .cloned()
        .collect()
}

/// The file and the engine's partial data and state next to it (for streams, the folder
/// of segments).
fn delete_files_of(path: &Path) {
    let with = |suffix: &str| {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        PathBuf::from(p)
    };
    for suffix in ["", ".shardload-part", ".shardload-state"] {
        std::fs::remove_file(with(suffix)).ok();
    }
    std::fs::remove_dir_all(with(".shardload-hls")).ok();
    // yt-dlp's partial files: `<stem>.f137.mp4.part`, `<stem>.mp4.ytdl` and the like.
    if let (Some(dir), Some(stem)) = (path.parent(), path.file_stem()) {
        let prefix = format!("{}.", stem.to_string_lossy());
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let partial = name.ends_with(".part") || name.ends_with(".ytdl");
            let subtitles = name.ends_with(".vtt") || name.ends_with(".ttml");
            if name.starts_with(&prefix) && (partial || subtitles) {
                std::fs::remove_file(entry.path()).ok();
            }
        }
    }
}

/// Asks the task's URL about the file, or when it fails its mirrors in turn (a mirror
/// network may list an address before that mirror has the file). The first error is the
/// one reported.
async fn probe_any(
    task: &Task,
    opts: &Options,
) -> shardload_engine::Result<shardload_engine::Probe> {
    let first = match shardload_engine::probe(&task.url, &task.headers, opts).await {
        Ok(p) => return Ok(p),
        Err(e) => e,
    };
    for mirror in &task.mirrors {
        if let Ok(p) = shardload_engine::probe(mirror, &task.headers, opts).await {
            return Ok(p);
        }
    }
    Err(first)
}

/// The site a URL belongs to for saved logins: its host, with the port when it is not the
/// scheme's default.
pub fn site_of(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    Some(match u.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

/// A remembered login, as shown to the user (the password stays in the list).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SavedLogin {
    pub site: String,
    pub user: String,
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
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
        let torrents = torrent::Torrents::new(config.torrent.clone(), config.dir.clone());
        let inner = Arc::new(Inner {
            torrents,
            db,
            events: broadcast::channel(1024).0,
            global: Limiter::new(None),
            config: Mutex::new(config),
            running: Mutex::new(HashMap::new()),
            scheduling: tokio::sync::Mutex::new(()),
            naming: tokio::sync::Mutex::new(()),
            retries: Mutex::new(HashMap::new()),
            moving: Mutex::new(HashSet::new()),
            closed: AtomicBool::new(false),
            notes: Mutex::new(HashMap::new()),
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

    /// The folders downloads are saved into: the download folder and each category's folder.
    pub fn download_folders(&self) -> Vec<PathBuf> {
        let cfg = self.config();
        std::iter::once(cfg.dir)
            .chain(cfg.category_dirs.into_values())
            .collect()
    }

    /// Applies to downloads started from now on; a larger `max_active` starts queued ones.
    pub async fn set_config(&self, config: Config) -> Result<()> {
        *lock(&self.inner.config) = config;
        self.inner.fill().await
    }

    pub async fn add(&self, mut new: NewTask) -> Result<Task> {
        // A metalink: the file it describes, from its addresses, checked against its hash.
        if shardload_engine::metalink::is_metalink(&new.url) {
            let opts = lock(&self.inner.config).engine.clone();
            let m =
                shardload_engine::metalink::fetch_metalink(&new.url, &new.headers, &opts).await?;
            let mut urls = m
                .urls
                .into_iter()
                .take(shardload_engine::metalink::MAX_URLS);
            new.url = urls.next().unwrap_or_default();
            new.mirrors = urls.chain(std::mem::take(&mut new.mirrors)).collect();
            new.name = new.name.or(Some(m.name));
            new.checksum = new.checksum.or(m.checksum);
        }
        if let Some(c) = &new.checksum {
            c.parse::<Checksum>().map_err(Error::BadChecksum)?;
        }
        let name = new.name.as_deref().map(shardload_engine::name::sanitize);
        // A guess from the URL until the server names the file.
        let category = Category::for_name(
            &name
                .clone()
                .or_else(|| shardload_engine::name::from_url(&new.url))
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
                stream: new.stream.as_ref(),
                mirrors: &new.mirrors,
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
            // A torrent with several files, or files out of an archive: the folder chosen
            // for them on the first start.
            let folder = torrent::is_torrent(&task.url)
                || task.stream.as_ref().is_some_and(|s| s.archive.is_some());
            if folder && path.is_dir() {
                std::fs::remove_dir_all(path).ok();
            }
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
            // Files out of an archive: their folder, or they would count as done.
            if task.stream.as_ref().is_some_and(|s| s.archive.is_some()) && path.is_dir() {
                std::fs::remove_dir_all(path).ok();
            }
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
    ) -> Result<shardload_engine::Probe> {
        let opts = lock(&self.inner.config).engine.clone();
        Ok(shardload_engine::probe(url, headers, &opts).await?)
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

    /// Gives a download another file name, cleaned like a name from a server; see
    /// [`Manager::relocate`] for how its files follow. A torrent's or an archive's folder is
    /// renamed; their names are known only once they start, so before that this fails.
    pub async fn rename(&self, id: TaskId, name: &str) -> Result<Task> {
        let name = shardload_engine::name::sanitize(name);
        self.place(id, None, Some(name)).await
    }

    /// Moves a download into another folder.
    ///
    /// A download with nothing on disk yet only changes in the list. Otherwise its files go
    /// along: the finished file, or the partial data and state of an unfinished one (a
    /// running download is paused for it and continues where it was), with a stream's
    /// subtitles and a torrent's or an archive's whole folder. On the same drive this is a
    /// rename; onto another drive the files are copied, checked and only then deleted, with
    /// [`Event::Moving`] as it goes. A name taken in the new place gets a number,
    /// `name (1).ext`.
    pub async fn relocate(&self, id: TaskId, dir: &Path) -> Result<Task> {
        self.place(id, Some(dir.to_owned()), None).await
    }

    /// Whether some download's files are being renamed or moved right now.
    pub fn is_moving(&self) -> bool {
        !lock(&self.inner.moving).is_empty()
    }

    async fn place(&self, id: TaskId, dir: Option<PathBuf>, name: Option<String>) -> Result<Task> {
        self.get(id).await?;
        if !lock(&self.inner.moving).insert(id) {
            return Err(Error::CannotMove(
                "its files are being moved already".into(),
            ));
        }
        // A queue fill that read the download as queued just before may be starting it;
        // once that is over, nothing starts it until the move is done.
        drop(self.inner.scheduling.lock().await);
        let was_running = self.inner.stop(id, Stop::Pause).await;
        let result = self.place_stopped(id, dir, name).await;
        lock(&self.inner.moving).remove(&id);
        if was_running {
            self.inner.db.set_status(id, Status::Queued, None).await?;
        }
        // Also after a failure: the list stops showing the move.
        let task = self.get(id).await?;
        self.inner.emit(Event::Updated(task.clone()));
        self.inner.fill().await?;
        result.map(|()| task)
    }

    async fn place_stopped(
        &self,
        id: TaskId,
        dir: Option<PathBuf>,
        name: Option<String>,
    ) -> Result<()> {
        let task = self.get(id).await?;
        let dir = dir.unwrap_or_else(|| task.dir.clone());
        let mut stream = task.stream.clone();
        let is_torrent = torrent::is_torrent(&task.url);
        let torrent_folder = stream
            .as_ref()
            .and_then(|s| s.torrent.as_ref())
            .and_then(|c| c.folder.clone());
        let archive = stream.as_ref().and_then(|s| s.archive.as_ref());
        let archive_folder = archive.and_then(|c| c.folder.clone());
        let named_on_start = is_torrent || archive.is_some_and(|a| !a.whole);

        let Some(path) = task.path.clone() else {
            // Not started: the name and folder are used when it starts.
            if name.is_some() && named_on_start {
                return Err(Error::CannotMove(
                    "its name is known once it starts; rename it then".into(),
                ));
            }
            let category = match &name {
                Some(n) => Category::for_name(n),
                None => task.category,
            };
            let name = name.or(task.name);
            return self
                .inner
                .db
                .set_place(id, &dir, name.as_deref(), None, category)
                .await;
        };

        // What moves: a torrent's or an archive's own folder, or the file.
        let own_folder = torrent_folder.or(archive_folder).filter(|f| *f != task.dir);
        let unit = own_folder.clone().unwrap_or_else(|| path.clone());
        let unit_name = name.clone().unwrap_or_else(|| file_name(&unit));
        // An unfinished torrent's file straight in the folder must keep the torrent's name.
        let fixed_name = is_torrent && own_folder.is_none() && task.status != Status::Done;
        if fixed_name && name.is_some() {
            return Err(Error::CannotMove(
                "a torrent's file can be renamed once it is finished".into(),
            ));
        }

        let _one_at_a_time = self.inner.naming.lock().await;
        let wanted = dir.join(&unit_name);
        let new_unit = if relocate::same_path(&wanted, &unit) {
            wanted.clone()
        } else {
            let taken: HashSet<PathBuf> =
                self.inner.db.paths_except(id).await?.into_iter().collect();
            shardload_engine::target_avoiding(&dir, &unit_name, "", Conflict::Rename, |p| {
                taken.contains(p)
            })
        };
        if new_unit == unit {
            return Ok(());
        }
        if fixed_name && new_unit != wanted {
            return Err(Error::CannotMove(format!(
                "{unit_name} is taken in that folder"
            )));
        }

        let members = if own_folder.is_some() {
            Some(unit.clone())
                .filter(|p| p.exists())
                .into_iter()
                .collect()
        } else {
            let stream = task
                .stream
                .as_ref()
                .is_some_and(|s| s.torrent.is_none() && s.archive.is_none());
            relocate::members(&unit, stream)
        };
        if members.is_empty() && task.status == Status::Done {
            return Err(Error::CannotMove(format!(
                "{} is no longer there",
                unit.display()
            )));
        }
        let moves: Vec<(PathBuf, PathBuf)> = members
            .into_iter()
            .map(|m| {
                let to = relocate::destination(&m, &unit, &new_unit);
                (m, to)
            })
            .collect();
        let events = self.inner.events.clone();
        tokio::task::spawn_blocking(move || {
            relocate::move_all(&moves, false, |copied, total| {
                let _ = events.send(Event::Moving(MoveProgress { id, copied, total }));
            })
        })
        .await
        .map_err(|e| shardload_engine::Error::from(std::io::Error::other(e)))?
        .map_err(shardload_engine::Error::from)?;

        let new_path = match path.strip_prefix(&unit) {
            Ok(rel) if !rel.as_os_str().is_empty() => new_unit.join(rel),
            _ => new_unit.clone(),
        };
        if let Some(s) = stream.as_mut() {
            if let Some(c) = s.torrent.as_mut() {
                c.folder = Some(match own_folder {
                    Some(_) => new_unit.clone(),
                    None => dir.clone(),
                });
            }
            if let Some(c) = s.archive.as_mut().filter(|c| c.folder.is_some()) {
                c.folder = Some(new_unit.clone());
            }
            self.inner.db.set_stream(id, s).await?;
        }
        let new_name = file_name(&new_unit);
        let category = if own_folder.is_none() && !is_torrent && name.is_some() {
            Category::for_name(&new_name)
        } else {
            task.category
        };
        self.inner
            .db
            .set_place(id, &dir, Some(&new_name), Some(&new_path), category)
            .await
    }

    /// Limit for all downloads together, bytes per second; takes effect at once.
    pub fn set_global_limit(&self, rate: Option<u64>) {
        self.inner.global.set_rate(rate);
        self.inner.torrents.set_limit(rate);
    }

    /// Reads `url` and the pages it links to, down to `opts.depth`, and returns the files
    /// they link to (see [`grabber`]), with the client settings downloads use.
    pub async fn grab_site(
        &self,
        url: &str,
        opts: &grabber::GrabOptions,
        progress: impl Fn(grabber::GrabProgress),
        cancel: &CancellationToken,
    ) -> Result<Vec<grabber::Found>> {
        let engine = lock(&self.inner.config).engine.clone();
        grabber::grab(url, opts, &[], &engine, progress, cancel)
            .await
            .map_err(|e| shardload_engine::Error::State(e).into())
    }

    /// The name and files of a torrent (a magnet link, or a `.torrent` file on the web or on
    /// disk), without adding it: for choosing files before the download starts.
    pub async fn inspect_torrent(&self, url: &str) -> Result<TorrentInfo> {
        self.inner
            .torrents
            .inspect(url)
            .await
            .map_err(|e| shardload_engine::Error::State(e).into())
    }

    /// The files in a ZIP archive on the server, read without downloading it (the server must
    /// support range requests; [`shardload_engine::Error::NoRanges`] otherwise): for choosing
    /// which to download.
    pub async fn inspect_archive(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<ArchiveInfo> {
        let opts = lock(&self.inner.config).engine.clone();
        let zip = shardload_engine::RemoteZip::open(url, headers, &opts).await?;
        Ok(ArchiveInfo {
            name: zip.name().to_owned(),
            size: zip.size(),
            zip64: zip.is_zip64(),
            entries: zip
                .entries()
                .iter()
                .map(|e| ArchiveEntry {
                    name: e.name.clone(),
                    size: e.size,
                    compressed_size: e.compressed_size,
                    method: shardload_engine::method_name(e.method),
                    is_dir: e.is_dir,
                    encrypted: e.encrypted,
                    unsupported: e.unsupported(),
                })
                .collect(),
        })
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
    /// with [`shardload_engine::Error::FileChanged`] and nothing changes. A download that was
    /// running or had failed continues with the new link.
    ///
    /// `headers` from the browser (fresh cookies, referrer) replace the download's headers of
    /// the same names; others, such as a login, are kept.
    pub async fn refresh_link(
        &self,
        id: TaskId,
        url: &str,
        headers: Option<&[(String, String)]>,
    ) -> Result<()> {
        let task = self.get(id).await?;
        let headers = match headers {
            Some(fresh) => merge_headers(&task.headers, fresh),
            None => task.headers.clone(),
        };
        let was_running = self.inner.stop(id, Stop::Pause).await;
        if let Some(path) = &task.path {
            let mut state = path.as_os_str().to_owned();
            state.push(".shardload-state");
            if PathBuf::from(state).exists() {
                let opts = lock(&self.inner.config).engine.clone();
                let req = shardload_engine::Request {
                    url: url.to_owned(),
                    output: path.clone(),
                    headers: headers.clone(),
                    mirrors: Vec::new(),
                };
                if let Err(e) = shardload_engine::patch_url(&req, &opts).await {
                    if was_running {
                        self.start(id).await?;
                    }
                    return Err(e.into());
                }
            }
        }
        self.inner.db.set_url(id, url).await?;
        self.inner.db.set_headers(id, &headers).await?;
        self.inner.emit(Event::Updated(self.get(id).await?));
        if was_running || task.status == Status::Error {
            self.start(id).await?;
        }
        Ok(())
    }

    /// Sends these credentials (HTTP Basic) with every request of the download from now on,
    /// replacing earlier ones, and queues it: for a server that asked for a login.
    pub async fn set_login(&self, id: TaskId, user: &str, password: &str) -> Result<()> {
        let task = self.get(id).await?;
        let was_running = self.inner.stop(id, Stop::Pause).await;
        let mut headers: Vec<(String, String)> = task
            .headers
            .into_iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("authorization"))
            .collect();
        headers.push(shardload_engine::basic_auth(user, password));
        self.inner.db.set_headers(id, &headers).await?;
        if was_running || task.status != Status::Done {
            self.inner.set_status(id, Status::Queued, None).await?;
        }
        self.inner.fill().await
    }

    /// Remembers a login for `site` (see [`site_of`]); a download there that the server asks
    /// to log in to uses it without asking.
    pub async fn save_login(&self, site: &str, user: &str, password: &str) -> Result<()> {
        self.inner.db.save_login(site, user, password).await
    }

    /// The login saved for the site of `url`.
    pub async fn saved_login(&self, url: &str) -> Result<Option<(String, String)>> {
        match site_of(url) {
            Some(site) => self.inner.db.login(&site).await,
            None => Ok(None),
        }
    }

    pub async fn saved_logins(&self) -> Result<Vec<SavedLogin>> {
        Ok(self
            .inner
            .db
            .logins()
            .await?
            .into_iter()
            .map(|(site, user)| SavedLogin { site, user })
            .collect())
    }

    pub async fn forget_login(&self, site: &str) -> Result<()> {
        self.inner.db.forget_login(site).await
    }

    /// Stops every running download so it resumes on the next [`Manager::open`], and closes
    /// the database.
    pub async fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        let ids: Vec<TaskId> = lock(&self.inner.running).keys().copied().collect();
        for id in ids {
            self.inner.stop(id, Stop::Shutdown).await;
        }
        self.inner.torrents.close().await;
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
                .chain(lock(&self.moving).iter().copied())
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
    async fn use_saved_login(&self, id: TaskId) -> bool {
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
    async fn run(
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
            (Ok(outcome), _) => {
                lock(&self.retries).remove(&id);
                match self.db.finish(id, outcome.size, now_ms()).await {
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
    }

    /// A stream (HLS or DASH): the name comes from the page title (or the playlist URL) with
    /// the extension of what will be written: `.mp4` with ffmpeg or for DASH (fragmented
    /// MP4), `.ts` for HLS without ffmpeg.
    async fn download_stream(
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
                let stem = shardload_engine::name::split_extension(&base).0.to_owned();
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
        let subtitles = stream.subtitles.clone();
        let is_dash = stream.dash.is_some();
        let video_path = path.clone();
        let (tx, rx) = watch::channel(Progress::default());
        let reporter = tokio::spawn(report(task.id, rx, self.events.clone(), self.db.clone()));
        let result = match stream.dash {
            Some(video) => {
                let req = shardload_engine::DashRequest {
                    manifest: task.url.clone(),
                    video,
                    audio: stream.audio,
                    output: path,
                    headers: task.headers.clone(),
                    ffmpeg,
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
    async fn download_archive(
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
        let reporter = tokio::spawn(report(task.id, rx, self.events.clone(), self.db.clone()));
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

    /// A torrent: on the first start its files are listed (for a magnet link, asked from
    /// peers) to pick where they go: a single file straight into the download folder, several
    /// into a folder named after the torrent. The choice is kept for later starts.
    async fn download_torrent(
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
        let reporter = tokio::spawn(report(task.id, rx, self.events.clone(), self.db.clone()));
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
            }),
            Err(_) if cancel.is_cancelled() => Err(shardload_engine::Error::Cancelled.into()),
            Err(e) => Err(state(e)),
        }
    }

    /// A page of a site yt-dlp knows: yt-dlp downloads the chosen format; the file's real
    /// extension is known only at the end. `mark` gives the file the Mark of the Web, with
    /// the page as its address.
    async fn download_page(
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
                let stem = shardload_engine::name::split_extension(&stem).0.to_owned();
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
        let reporter = tokio::spawn(report(task.id, rx, self.events.clone(), self.db.clone()));
        let result = ytdlp::run(tools, &task.url, format, &stem, &task.headers, &tx, cancel).await;
        drop(tx);
        reporter.await.ok();
        let file = match result {
            Ok(f) => f,
            Err(_) if cancel.is_cancelled() => {
                return Err(shardload_engine::Error::Cancelled.into());
            }
            Err(e) => return Err(shardload_engine::Error::State(e).into()),
        };
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
        })
    }

    /// After a failure: when it may pass and retries are left, records when to try again,
    /// wakes the queue then, and returns the wait and whether the machine is offline.
    fn schedule_retry(self: &Arc<Self>, id: TaskId, e: &Error) -> Option<(Duration, bool)> {
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
    async fn set_up_ffmpeg(
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

    /// Keeps `note` with the download (see [`Inner::notes`]) and shows it now.
    async fn note(&self, task: &mut Task, note: String) -> Result<()> {
        self.db
            .set_status(task.id, Status::Downloading, Some(&note))
            .await?;
        task.error = Some(note.clone());
        lock(&self.notes).insert(task.id, note);
        self.emit(Event::Updated(task.clone()));
        Ok(())
    }

    async fn download(
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
            let fallback = no_ffmpeg.filter(|_| ytdlp::merges(&format));
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
                        "this video only comes as separate video and audio, and ffmpeg, which                          joins them, could not be set up ({why}); start the download again to                          try once more"
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

        let path = match &task.path {
            Some(p) => p.clone(),
            None => {
                // First start: ask the server for the name and size, then pick a free path.
                let probe = tokio::select! {
                    p = probe_any(&task, &opts) => p?,
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
        let reporter = tokio::spawn(report(task.id, rx, self.events.clone(), self.db.clone()));
        let result = shardload_engine::download(&req, &opts, Some(&tx), cancel).await;
        drop(tx);
        reporter.await.ok();
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
            checksum_from: p.checksum_from,
            segments: p.segments,
        }));
    }
}
