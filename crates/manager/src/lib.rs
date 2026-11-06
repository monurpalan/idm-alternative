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

pub mod after;
mod api_control;
mod api_list;
mod api_settings;
pub mod batch;
pub mod clip;
pub mod command;
mod db;
pub mod deadline;
mod dedup;
mod delta;
pub mod disk;
pub mod eta;
pub mod folder;
pub mod grabber;
pub mod hints;
pub mod net;
pub mod origin;
mod pages;
mod peek;
mod previous;
mod queue;
mod refresh;
mod relocate;
pub mod rescue;
mod retry;
mod secret;
pub mod sites;
mod streams;
mod task;
pub mod torrent;
mod transfer;
pub mod wayback;
pub mod ytdlp;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use shardload_engine::{
    CancellationToken, Checksum, Conflict, Limiter, Options, Outcome, Phase, Progress,
};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use db::{Db, Insert};
pub use dedup::{Reuse, same_volume};
pub(crate) use previous::on_disk;
pub use previous::{Matched, Previous};
pub use secret::{open_setting, seal_setting};
pub use sites::SiteMemory;
pub use task::{
    ArchiveChoice, Category, ErrorKind, Group, HistoryEntry, NewTask, Recording, Status, Stream,
    Structure, Subtitle, Task, TaskId, TaskProgress, TorrentChoice, Warning,
};
pub use torrent::{TorrentConfig, TorrentFile, TorrentInfo};
use transfer::report;

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
    /// A clip whose times cannot be used.
    #[error("{0}")]
    BadClip(String),
    /// A `.torrent` given to check a download that cannot be used.
    #[error("{0}")]
    BadTorrent(String),
    /// Request headers that cannot be sent (see [`command::check_headers`]).
    #[error("{0}")]
    BadHeaders(String),
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
    /// The drive a folder is on and the space free there (see [`disk::FreeSpace`]); `None`
    /// reads the real disks.
    pub free_space: Option<Space>,
    /// How often the drives running downloads write to are looked at.
    pub disk_watch: Duration,
    /// The user's rules for finished downloads (see [`after`]).
    pub rules: Vec<after::Rule>,
    pub after_limits: after::Limits,
    /// What the network and the power are like (see [`net`]); `None` takes them as an
    /// unmetered network on mains power.
    pub sensor: Option<net::Sensor>,
    pub net_rules: net::NetRules,
    /// How often the network and the power are looked at.
    pub sense_every: Duration,
    /// Read an archive's, a video's or a document's skeleton when it starts (see `peek`).
    pub skeleton_first: bool,
}

/// A [`disk::FreeSpace`] in the configuration.
#[derive(Clone)]
pub struct Space(pub disk::FreeSpace);

impl std::fmt::Debug for Space {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Space")
    }
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
            free_space: None,
            disk_watch: Duration::from_secs(2),
            rules: Vec::new(),
            after_limits: after::Limits::default(),
            sensor: None,
            net_rules: net::NetRules::default(),
            sense_every: Duration::from_secs(15),
            skeleton_first: true,
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
    /// A drive came within [`disk::RESERVE`] of full: the downloads writing to it (and
    /// those waiting to) were paused.
    DiskLow(disk::DiskLow),
    /// The monthly data budget is nearly or wholly used (see [`net`]).
    Budget(net::BudgetNotice),
    /// A download will not make its deadline at the speed now (see [`deadline`]).
    DeadlineRisk(deadline::DeadlineRisk),
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
    /// Queued recordings that start at a set time (Unix milliseconds), not before.
    later: Mutex<HashMap<TaskId, i64>>,
    /// The network and the power as last seen, and this month's data (see [`net`]).
    net: Mutex<net::NetState>,
    /// What the deadline watch keeps (see [`deadline`]).
    deadlines: Mutex<deadline::DeadlineState>,
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
        Some(format) => ytdlp::merges(format) || stream.clip.is_some(),
        None => {
            (stream.audio.is_some() || stream.clip.is_some())
                && stream.torrent.is_none()
                && stream.archive.is_none()
        }
    }
}

/// `stem` with the clip's range after it: `film (1.05-2.30)`.
fn clip_stem(stem: &str, stream: &Stream) -> String {
    match &stream.clip {
        Some(c) => format!("{stem}{}", clip::label(c)),
        None => stem.to_owned(),
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
            | E::Remote(_)
            | E::Busy(..)
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

/// A waiting download whose link may soon stop working (see [`Manager::link_warnings`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct LinkWarning {
    pub id: TaskId,
    /// When the link is expected to stop working, Unix milliseconds.
    pub expires_at: i64,
    /// Already past that: it most likely needs a fresh link from the browser.
    pub expired: bool,
}

impl LinkWarning {
    /// For a link given at `given` (Unix ms) on a site whose links lasted `lifetime` seconds:
    /// a warning once three quarters of that have passed.
    fn of(id: TaskId, given: i64, lifetime: u64, now: i64) -> Option<Self> {
        let lifetime = (lifetime as i64).saturating_mul(1000);
        let expires_at = given.saturating_add(lifetime);
        (now - given >= lifetime / 4 * 3).then_some(Self {
            id,
            expires_at,
            expired: now >= expires_at,
        })
    }
}

/// `Name (2).ext` → `name.ext`: the names downloads of the same file may have got.
pub(crate) fn plain_name(name: &str) -> String {
    let (stem, ext) = shardload_engine::name::split_extension(name);
    let stem = match stem.rsplit_once(" (") {
        Some((base, n))
            if n.strip_suffix(')')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) =>
        {
            base
        }
        _ => stem,
    };
    format!("{stem}{ext}").to_lowercase()
}

/// The address without its `#fragment`, which never reaches the server.
fn without_fragment(url: &str) -> &str {
    url.split_once('#').map_or(url, |(u, _)| u)
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

/// Now, Unix milliseconds.
pub fn unix_ms() -> i64 {
    now_ms()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
