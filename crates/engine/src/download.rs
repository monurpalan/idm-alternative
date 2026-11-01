//! One download: probe, resume or start fresh, run connections, checkpoint, finish.

mod connection;
mod ctx;
mod job;

use connection::*;
use ctx::*;
use job::*;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use wreq::Response;

use crate::checksum::Checksum;
use crate::error::{Error, Result};
use crate::file::OutputFile;
use crate::limit::Limiter;
use crate::remote::{Browser, Http, HttpConfig, Remote};
use crate::segments::{SplitPolicy, Table};
use crate::state::{State, sibling};

/// Bytes a connection collects before handing them to the OS in one write.
const WRITE_CHUNK: usize = 1 << 20;
/// A connection's buffer is written at least this often, so checkpoints keep moving.
const WRITE_EVERY: Duration = Duration::from_secs(1);
const TICK: Duration = Duration::from_millis(100);
/// Under a speed limit, at most one connection per this many bytes/s: connections that
/// trickle are more likely to be dropped by servers and gain nothing.
const MIN_LIMITED_CONN_RATE: u64 = 32 << 10;
/// Failures in a row after which a mirror is dropped.
const MIRROR_FAILURES: u32 = 3;
/// Crawling connections of a mirror after which it is dropped: the controller only calls a
/// connection crawling when it has stayed far behind the others for seconds.
const MIRROR_SLOW: u32 = 1;
/// How long a mirror's connections run before their speed is judged, and the share of the
/// best connection speed below which the mirror is dropped.
const SETTLED: Duration = Duration::from_secs(2);
/// How long a mirror may take to send the start of the file before it is left out.
const MIRROR_CHECK: Duration = Duration::from_secs(3);
const SLOW_MIRROR: f64 = 0.15;
/// A connection is moved off its source when that source gives a connection less than this
/// share of what the best one does; at most one move per [`MOVE_EVERY`].
const MOVE_BELOW: f64 = 0.5;
const MOVE_EVERY: Duration = Duration::from_secs(1);
/// How long a connection runs before it may be moved: its speed is known by then.
const MOVE_SETTLED: Duration = Duration::from_secs(1);
/// What a move must save: a new connection and its ramp-up cost something.
const MOVE_GAIN: Duration = Duration::from_secs(1);
/// Bytes from the start of the file compared between the download's URL and each mirror.
const MIRROR_SAMPLE: u64 = 64 << 10;

/// Files smaller than this are not worth looking for a published checksum for.
const FIND_CHECKSUM_FROM: u64 = 1 << 20;

/// Bounds of a request's size under a speed limit (see [`Ctx::window_end`]).
const MIN_WINDOW: u64 = 256 << 10;
const MAX_WINDOW: u64 = 16 << 20;

#[derive(Debug, Clone)]
pub struct Request {
    pub url: String,
    /// Final file path. While downloading, data goes to `<output>.shardload-part`, resume
    /// information to `<output>.shardload-state`, and `<output>.shardload-lock` keeps a second
    /// download of the same file away. See [`crate::target`] for choosing a free path.
    pub output: PathBuf,
    pub headers: Vec<(String, String)>,
    /// Other addresses of the same file (mirrors). Each is used alongside `url` once it is
    /// shown to serve the same bytes; one that refuses, keeps failing or crawls is dropped,
    /// and the download goes on without it.
    pub mirrors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Maximum simultaneous connections.
    pub connections: usize,
    /// A connection that receives nothing for this long is dropped and its range reassigned.
    pub read_timeout: Duration,
    /// How often durable progress is written to the state file.
    pub checkpoint_every: Duration,
    /// Consecutive failures of one segment, without progress, before giving up.
    pub max_failures: u32,
    /// `None` sends the browser's User-Agent, or the engine's without a browser.
    pub user_agent: Option<String>,
    /// TLS, HTTP/2 and header fingerprint to present; `None` is a plain client.
    pub browser: Option<Browser>,
    /// `http://`, `https://` or `socks5://` proxy for every request.
    pub proxy: Option<String>,
    /// Accept invalid or self-signed certificates.
    pub insecure: bool,
    /// Never negotiate HTTP/2.
    pub http1_only: bool,
    pub split: SplitPolicy,
    /// Speed limits this download obeys, typically the app's global limiter and the
    /// download's own. Every connection pays each of them for every byte.
    pub limiters: Vec<Limiter>,
    /// Checked once the file is complete; a mismatch fails the download.
    pub checksum: Option<Checksum>,
    /// On Windows, mark the finished file as downloaded from the Internet (the
    /// `Zone.Identifier` stream browsers write), so SmartScreen and Office's Protected View
    /// treat it as they would a browser download. See [`crate::mark_of_the_web`].
    pub mark_of_the_web: bool,
    /// Where `checksum` came from when the user did not give it (e.g. `SHA256SUMS`), for
    /// messages.
    pub checksum_from: Option<String>,
    /// Without `checksum`, look for a published one while downloading: GitHub's digest of a
    /// release asset, or a checksum file next to the file (`<file>.sha256`, `SHA256SUMS`…).
    /// A checksum found is checked like a given one.
    pub find_checksum: bool,
    /// Without `checksum`: one shown on the page the link was on, checked when the publisher
    /// posted none (a published one comes first). Reported as from [`crate::FROM_PAGE`].
    pub page_checksum: Option<Checksum>,
    /// Where the download notes what it learned about the server (see [`Learned`]); each
    /// clone of the options shares it.
    pub learn: Learning,
    /// Hashes of the file's pieces: once all is downloaded each piece is checked, and the
    /// corrupt ones are downloaded again on their own (before `checksum` is checked).
    pub pieces: Option<crate::pieces::Pieces>,
}

/// What a download found out about a server, for starting the next download from it right
/// away: kept by the caller per site.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Learned {
    /// The server refused connections beyond this many (403, 429, 503 while others worked).
    pub max_connections: Option<usize>,
    /// The most connections that were downloading at once.
    pub peak_connections: usize,
    /// The server does not send parts of the file (range requests), so one connection
    /// downloads it from the start.
    pub no_ranges: bool,
}

/// A shared [`Learned`]: give the download options one, read it when the download ends.
#[derive(Debug, Clone, Default)]
pub struct Learning(std::sync::Arc<std::sync::Mutex<Learned>>);

impl Learning {
    pub fn get(&self) -> Learned {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn note(&self, f: impl FnOnce(&mut Learned)) {
        f(&mut self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner));
    }

    fn limited_to(&self, n: usize) {
        self.note(|l| l.max_connections = Some(l.max_connections.map_or(n, |m| m.min(n))));
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            connections: 16,
            read_timeout: Duration::from_secs(20),
            checkpoint_every: Duration::from_secs(2),
            max_failures: 8,
            user_agent: None,
            browser: Some(Browser::Chrome),
            proxy: None,
            insecure: false,
            http1_only: false,
            split: SplitPolicy::default(),
            limiters: Vec::new(),
            checksum: None,
            mark_of_the_web: true,
            checksum_from: None,
            find_checksum: true,
            page_checksum: None,
            learn: Learning::default(),
            pieces: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SegmentView {
    pub start: u64,
    pub end: u64,
    pub received: u64,
    pub active: bool,
    pub speed: f64,
}

/// What a download is doing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Phase {
    /// Asking the server about the file.
    #[default]
    Probing,
    /// Setting up a helper program the download needs first (the manager's ffmpeg); the
    /// sizes are the helper's.
    SettingUp,
    Downloading,
    /// Hashing the finished file; see [`Progress::verified`].
    Verifying,
    /// Recording a live stream: no size, [`Progress::recorded`] grows until it stops.
    Recording,
    Done,
}

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub phase: Phase,
    pub size: Option<u64>,
    pub received: u64,
    /// Bytes per second over the last moments.
    pub speed: f64,
    pub connections: usize,
    pub segments: Vec<SegmentView>,
    /// Bytes hashed so far while [`Phase::Verifying`].
    pub verified: u64,
    /// While [`Phase::Verifying`]: where the checksum came from, `None` when the user gave
    /// it.
    pub checksum_from: Option<String>,
    /// While [`Phase::Recording`]: seconds of the stream recorded.
    pub recorded: f64,
    /// While [`Phase::Recording`]: segments lost because they left the live playlist before
    /// they were read.
    pub missed: u64,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub size: u64,
    /// Bytes already on disk from an earlier attempt.
    pub resumed_from: u64,
    pub elapsed: Duration,
    /// The checksum the file was verified against, if any.
    pub checksum: Option<Checksum>,
    /// Where that checksum came from (e.g. `SHA256SUMS`, `GitHub release`); `None` when the
    /// user gave it.
    pub checksum_from: Option<String>,
}

/// Downloads `req.url` to `req.output`, resuming a previous attempt when possible.
///
/// On error or cancellation the partial file and the state file are kept (except when the
/// remote file changed), so calling again resumes.
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "one long sequence kept together; to be split when it next changes"
)]
pub async fn download(
    req: &Request,
    opts: &Options,
    progress: Option<&watch::Sender<Progress>>,
    cancel: &CancellationToken,
) -> Result<Outcome> {
    let started = Instant::now();
    let _lock = crate::place::Lock::acquire(&req.output)?;
    let http = Http::new(&http_config(&req.headers, opts))?;
    let part = sibling(&req.output, ".shardload-part");
    let state_path = sibling(&req.output, ".shardload-state");
    let mut force_single = false;
    // No checksum given: look for a published one meanwhile, once the server has said the
    // file is not small. Dropping the set (the download failed or was cancelled) stops the
    // search.
    let (size_known, mut size_rx) = watch::channel(None::<Option<u64>>);
    let mut discovery = JoinSet::new();
    if opts.checksum.is_none() && opts.find_checksum {
        let names: Vec<String> = crate::name::from_url(&req.url)
            .into_iter()
            .chain(
                req.output
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned()),
            )
            .collect();
        let search =
            crate::discover::discover(http.clone(), http_config(&[], opts), req.url.clone(), names);
        discovery.spawn(async move {
            let size = (*size_rx.wait_for(Option::is_some).await.ok()?).flatten();
            if size.is_some_and(|s| s < FIND_CHECKSUM_FROM) {
                return None;
            }
            search.await
        });
    }
    // With mirrors, the URL is just the first address: when it fails, the next one takes
    // its place (a partial file carries over once the new address serves the same bytes).
    let mut current = req.clone();
    loop {
        let job = Job {
            req: &current,
            opts,
            http: &http,
            part: &part,
            state_path: &state_path,
            force_single,
            size_known: &size_known,
        };
        match job.run(progress, cancel).await {
            Ok((size, resumed_from)) => {
                if let Some(pieces) = &opts.pieces {
                    let urls: Vec<String> = std::iter::once(current.url.clone())
                        .chain(current.mirrors.iter().cloned())
                        .collect();
                    crate::pieces::repair(&http, &urls, &part, size, pieces, progress, cancel)
                        .await?;
                }
                let (checksum, checksum_from) = match &opts.checksum {
                    Some(c) => (Some(c.clone()), opts.checksum_from.clone()),
                    None => {
                        let found = tokio::select! {
                            f = discovery.join_next() => f.and_then(Result::ok).flatten(),
                            () = cancel.cancelled() => return Err(Error::Cancelled),
                        };
                        match (found, &opts.page_checksum) {
                            (Some(f), _) => (Some(f.checksum), Some(f.from)),
                            (None, Some(page)) => {
                                (Some(page.clone()), Some(crate::FROM_PAGE.to_owned()))
                            }
                            (None, None) => (None, None),
                        }
                    }
                };
                if let Some(want) = &checksum {
                    match verify(&part, want, checksum_from.as_deref(), progress, cancel).await {
                        // Interrupted: the complete part and its state stay, so the next
                        // attempt only verifies again.
                        Err(Error::Cancelled) => return Err(Error::Cancelled),
                        // Keep the file for inspection; downloading again starts over.
                        Err(Error::ChecksumMismatch {
                            expected, actual, ..
                        }) => {
                            std::fs::rename(&part, &req.output)?;
                            std::fs::remove_file(&state_path).ok();
                            mark(&req.output, &current, opts);
                            return Err(Error::ChecksumMismatch {
                                expected,
                                actual,
                                origin: checksum_from,
                            });
                        }
                        other => other?,
                    }
                }
                std::fs::rename(&part, &req.output)?;
                std::fs::remove_file(&state_path).ok();
                mark(&req.output, &current, opts);
                if let Some(p) = progress {
                    p.send_modify(|p| p.phase = Phase::Done);
                }
                return Ok(Outcome {
                    size,
                    resumed_from,
                    elapsed: started.elapsed(),
                    checksum,
                    checksum_from,
                });
            }
            Err(Error::RangeIgnored) if !force_single => {
                opts.learn.note(|l| l.no_ranges = true);
                tracing::warn!("server ignores range requests; restarting with one connection");
                discard(&part, &state_path);
                force_single = true;
            }
            Err(Error::FileChanged) => {
                discard(&part, &state_path);
                return Err(Error::FileChanged);
            }
            Err(e) if !current.mirrors.is_empty() && another_address_may_help(&e) => {
                let next = current.mirrors.remove(0);
                tracing::warn!(failed = %current.url, next = %next, "{e}; continuing from a mirror");
                current.url = next;
            }
            Err(e) => return Err(e),
        }
    }
}

/// The Mark of the Web for a finished file, with the address it came from in the end (a
/// mirror, when the download's own URL failed).
fn mark(output: &Path, req: &Request, opts: &Options) {
    if opts.mark_of_the_web {
        crate::mark_of_the_web(output, &req.url, &req.headers);
    }
}

/// Whether an error is the address's own, so another address of the file may work: not
/// one of the disk, the file or the user's.
fn another_address_may_help(e: &Error) -> bool {
    matches!(
        e,
        Error::Http(_)
            | Error::Status(_)
            | Error::LinkExpired(_)
            | Error::Busy(_)
            | Error::HtmlResponse
            | Error::BadRange(_)
            | Error::Truncated
            | Error::Stalled(_)
            | Error::TooManyFailures { .. }
    )
}

/// What a server says about a URL before anything is downloaded, for deciding where to save
/// it and showing it to the user.
#[derive(Debug, Clone)]
pub struct Probe {
    /// URL after redirects.
    pub url: String,
    pub size: Option<u64>,
    /// Range requests work: the download can use several connections and resume.
    pub resumable: bool,
    /// Safe file name from `Content-Disposition`, the URL or the media type.
    pub name: String,
    /// The server's validators for the file, to tell later whether it changed.
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Asks the server about `url` with the same client settings a download would use, reading
/// only the response headers.
pub async fn probe(url: &str, headers: &[(String, String)], opts: &Options) -> Result<Probe> {
    let http = Http::new(&http_config(headers, opts))?;
    let (remote, resp) = http.probe(url, 0, None, 0).await?;
    drop(resp);
    Ok(Probe {
        url: remote.url,
        size: remote.size,
        resumable: remote.ranges && remote.size.is_some(),
        name: remote.name,
        etag: remote.validators.etag,
        last_modified: remote.validators.last_modified,
    })
}

/// Looks for a checksum the publisher posted for the file at `url` (named `name` on disk):
/// GitHub's digest of a release asset, or a checksum file next to it. Returns it with
/// where it was found.
pub async fn find_published_checksum(
    url: &str,
    name: &str,
    headers: &[(String, String)],
    opts: &Options,
) -> Result<Option<(Checksum, String)>> {
    let http = Http::new(&http_config(headers, opts))?;
    let names: Vec<String> = crate::name::from_url(url)
        .into_iter()
        .chain(std::iter::once(name.to_owned()))
        .collect();
    Ok(
        crate::discover::discover(http, http_config(&[], opts), url.to_owned(), names)
            .await
            .map(|f| (f.checksum, f.from)),
    )
}

/// For a GitHub release asset (`…/releases/download/<tag>/<asset>`): the release it is from
/// and the newest release of the project, when GitHub says.
pub async fn github_releases(url: &str, opts: &Options) -> Result<Option<(String, String)>> {
    let Some(release) = crate::discover::github_release(url) else {
        return Ok(None);
    };
    let Some(tag) = release.tag.clone() else {
        return Ok(None);
    };
    let api = format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        release.owner, release.repo
    );
    let headers = [(
        "Accept".to_owned(),
        "application/vnd.github+json".to_owned(),
    )];
    let (json, _) = fetch_text(&api, &headers, opts, 4 << 20).await?;
    let latest: serde_json::Value = serde_json::from_str(&json)
        .map_err(|e| Error::State(format!("GitHub answered something unexpected: {e}")))?;
    let tag = percent_decoded(&tag);
    Ok(latest["tag_name"]
        .as_str()
        .map(|latest| (tag, latest.to_owned())))
}

/// `v1%2B2` → `v1+2`.
fn percent_decoded(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={s}").as_bytes())
        .next()
        .map_or_else(|| s.to_owned(), |(_, v)| v.into_owned())
}

/// Hashes the finished file, reporting progress about four times a second.
async fn verify(
    part: &Path,
    want: &Checksum,
    from: Option<&str>,
    progress: Option<&watch::Sender<Progress>>,
    cancel: &CancellationToken,
) -> Result<()> {
    let done = Arc::new(AtomicU64::new(0));
    let report = |done: u64| {
        if let Some(p) = progress {
            p.send_modify(|p| {
                p.phase = Phase::Verifying;
                p.speed = 0.0;
                p.connections = 0;
                p.verified = done;
                if p.checksum_from.as_deref() != from {
                    p.checksum_from = from.map(str::to_owned);
                }
            });
        }
    };
    report(0);
    let check = crate::checksum::verify(part, want, cancel, Arc::clone(&done));
    tokio::pin!(check);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            result = &mut check => {
                report(done.load(Ordering::Relaxed));
                return result;
            }
            _ = tick.tick() => report(done.load(Ordering::Relaxed)),
        }
    }
}

/// Fetches a small text resource such as a playlist with the same client settings a
/// download would use. Returns the text and the URL after redirects (relative links in a
/// playlist are relative to that).
pub async fn fetch_text(
    url: &str,
    headers: &[(String, String)],
    opts: &Options,
    limit: usize,
) -> Result<(String, String)> {
    Http::new(&http_config(headers, opts))?
        .text(url, limit)
        .await
}

/// Like [`fetch_text`], for a small file that is not text (a `.torrent`).
pub async fn fetch_bytes(
    url: &str,
    headers: &[(String, String)],
    opts: &Options,
    limit: usize,
) -> Result<Vec<u8>> {
    Ok(Http::new(&http_config(headers, opts))?
        .bytes(url, limit)
        .await?
        .0)
}

pub(crate) fn http_config(headers: &[(String, String)], opts: &Options) -> HttpConfig {
    HttpConfig {
        user_agent: opts.user_agent.clone(),
        browser: opts.browser,
        proxy: opts.proxy.clone(),
        insecure: opts.insecure,
        http1_only: opts.http1_only,
        headers: headers.to_vec(),
    }
}

fn discard(part: &Path, state: &Path) {
    std::fs::remove_file(part).ok();
    std::fs::remove_file(state).ok();
}
