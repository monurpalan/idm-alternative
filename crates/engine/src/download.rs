//! One download: probe, resume or start fresh, run connections, checkpoint, finish.

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
    /// Where the download notes what it learned about the server (see [`Learned`]); each
    /// clone of the options shares it.
    pub learn: Learning,
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
            learn: Learning::default(),
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
                let (checksum, checksum_from) = match &opts.checksum {
                    Some(c) => (Some(c.clone()), opts.checksum_from.clone()),
                    None => {
                        let found = tokio::select! {
                            f = discovery.join_next() => f.and_then(Result::ok).flatten(),
                            () = cancel.cancelled() => return Err(Error::Cancelled),
                        };
                        match found {
                            Some(f) => (Some(f.checksum), Some(f.from)),
                            None => (None, None),
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
    })
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

struct Job<'a> {
    req: &'a Request,
    opts: &'a Options,
    http: &'a Http,
    part: &'a Path,
    state_path: &'a Path,
    force_single: bool,
    /// Told the file's size (`Some(None)`: unknown) once the server has said.
    size_known: &'a watch::Sender<Option<Option<u64>>>,
}

/// Shared by the controller and every connection.
struct Ctx {
    http: Http,
    /// Where requests go now; replaced when an expired redirect target is resolved again.
    remote: RwLock<Remote>,
    /// Bumped on every replacement of `remote`.
    generation: AtomicU64,
    /// Held while resolving the original URL again, so connections that fail together
    /// trigger it once.
    relinking: tokio::sync::Mutex<()>,
    /// The URL the user asked for, before redirects.
    original_url: String,
    size: Option<u64>,
    ranges: bool,
    file: OutputFile,
    table: Mutex<Table>,
    read_timeout: Duration,
    limiters: Vec<Limiter>,
    /// How many connections should close at the end of their current request, because a
    /// speed limit needs fewer than are open.
    shed: AtomicUsize,
    /// Source 0 is `remote`; then one per mirror, in use once it is shown to serve the
    /// same bytes.
    sources: Mutex<Vec<Source>>,
    cancel: CancellationToken,
}

/// How one source of the file is doing: the download's own URL (0) or a mirror.
#[derive(Default)]
struct Source {
    /// A mirror's answer once it is checked; `None` while it is being checked (and for
    /// source 0, whose address is `Ctx::remote`).
    remote: Option<Remote>,
    active: usize,
    /// Failed requests in a row.
    failures: u32,
    /// Connections dropped for crawling.
    slow: u32,
    dropped: bool,
    /// A mirror got its first connection, which measures it.
    tried: bool,
    /// Bytes per second per connection, as last measured.
    speed: f64,
}

impl Ctx {
    fn table(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The lowest active speed limit, if any.
    fn tightest_limit(&self) -> Option<u64> {
        self.limiters.iter().filter_map(Limiter::rate).min()
    }

    /// Where the next request for `pos..end` should stop. Under a speed limit, requests ask
    /// for half a second of the limit at most: a throttled connection fills the buffers
    /// between it and the server, and whatever is buffered past the point where its range
    /// shrinks or it closes is thrown away. Small requests bound that loss and let a
    /// connection close between two of them without any.
    fn window_end(&self, pos: u64, end: u64) -> u64 {
        match self.tightest_limit() {
            Some(rate) if self.ranges => end.min(pos + (rate / 2).clamp(MIN_WINDOW, MAX_WINDOW)),
            _ => end,
        }
    }

    /// Whether this connection should close now, claiming one of the requested closes.
    fn take_shed(&self) -> bool {
        self.shed
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .is_ok()
    }

    fn sources(&self) -> MutexGuard<'_, Vec<Source>> {
        self.sources.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The source for a new connection. A mirror just shown to serve the file gets one
    /// connection to measure it; after that each connection goes to the source that gives a
    /// connection the most, so mirrors only take connections where they add speed (a server
    /// that limits each client gives each connection less as more of them ask).
    fn pick_source(&self) -> usize {
        let mut sources = self.sources();
        let ready = |(i, s): &(usize, &Source)| !s.dropped && (*i == 0 || s.remote.is_some());
        let untried = sources
            .iter()
            .enumerate()
            .filter(ready)
            .find(|(i, s)| *i > 0 && !s.tried)
            .map(|(i, _)| i);
        let best = untried.unwrap_or_else(|| {
            sources
                .iter()
                .enumerate()
                .filter(ready)
                .max_by(|(i, a), (j, b)| {
                    a.speed
                        .total_cmp(&b.speed)
                        .then(b.active.cmp(&a.active))
                        .then(j.cmp(i))
                })
                .map_or(0, |(i, _)| i)
        });
        let s = &mut sources[best];
        s.active += 1;
        s.tried = true;
        best
    }

    /// Connections worth moving to a faster source: on a source that gives a connection
    /// less than [`MOVE_BELOW`] of the best source's, running for [`MOVE_SETTLED`] at least,
    /// and that would finish at least [`MOVE_GAIN`] sooner started again on the best source.
    fn connections_to_move(
        &self,
        t: &Table,
        running: &HashMap<usize, (CancellationToken, u64, usize)>,
        now: Instant,
    ) -> Vec<usize> {
        let Some((best, best_speed)) = ({
            let sources = self.sources();
            if sources.len() < 2 {
                return Vec::new();
            }
            sources
                .iter()
                .enumerate()
                .filter(|(i, s)| !s.dropped && (*i == 0 || s.remote.is_some()))
                .map(|(i, s)| (i, s.speed))
                .max_by(|a, b| a.1.total_cmp(&b.1))
        }) else {
            return Vec::new();
        };
        if best_speed <= 0.0 {
            return Vec::new();
        }
        let sources = self.sources();
        running
            .iter()
            .filter(|(_, (stop, _, source))| {
                *source != best
                    && !stop.is_cancelled()
                    && sources[*source].speed < MOVE_BELOW * best_speed
            })
            .map(|(&idx, _)| (idx, &t.segs[idx]))
            .filter(|(_, s)| {
                let left = s.remaining() as f64;
                s.owned_since.is_some_and(|at| now - at >= MOVE_SETTLED)
                    && s.speed > 0.0
                    && left / s.speed > left / best_speed + MOVE_GAIN.as_secs_f64()
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Records each source's speed per connection from the connections running now.
    fn measure_sources(
        &self,
        t: &Table,
        running: &HashMap<usize, (CancellationToken, u64, usize)>,
    ) {
        let mut sums: HashMap<usize, (f64, usize)> = HashMap::new();
        for (&idx, &(_, _, source)) in running {
            let speed = t.segs[idx].speed;
            if speed > 0.0 {
                let e = sums.entry(source).or_default();
                e.0 += speed;
                e.1 += 1;
            }
        }
        let mut sources = self.sources();
        for (source, (sum, n)) in sums {
            sources[source].speed = sum / n as f64;
        }
    }

    /// While mirrors are being checked, the connections the sources in use may open: all
    /// but a few kept for the mirrors to be measured with (at most a quarter of `limit`).
    /// No cap once all are checked.
    fn cap_while_checking(&self, limit: usize) -> usize {
        let pending = self
            .sources()
            .iter()
            .skip(1)
            .filter(|s| s.remote.is_none() && !s.dropped)
            .count();
        if pending == 0 {
            return usize::MAX;
        }
        limit.saturating_sub(pending.min(limit / 4)).max(1)
    }

    fn source_done(&self, source: usize, ok: bool) {
        let mut sources = self.sources();
        let s = &mut sources[source];
        s.active = s.active.saturating_sub(1);
        if ok {
            s.failures = 0;
        }
    }

    /// A request to `source` failed. A mirror is dropped when it refuses outright or keeps
    /// failing; the download's own URL never is (its errors are handled as before). Whether
    /// that dropped the source.
    fn source_failed(&self, source: usize, e: &Error) -> bool {
        let mut sources = self.sources();
        let s = &mut sources[source];
        s.failures += 1;
        let refused = e.is_fatal()
            || matches!(e, Error::RangeIgnored | Error::BadRange(_))
            || matches!(e, Error::Status(code) if (400..500).contains(code));
        if source > 0 && !s.dropped && (refused || s.failures >= MIRROR_FAILURES) {
            s.dropped = true;
            tracing::info!(mirror = %s.remote.as_ref().map_or("?", |r| r.url.as_str()), "dropping a mirror: {e}");
            return true;
        }
        false
    }

    /// A connection to `source` was dropped for crawling. Whether that dropped the source.
    fn source_slow(&self, source: usize) -> bool {
        let mut sources = self.sources();
        let s = &mut sources[source];
        s.slow += 1;
        if source > 0 && !s.dropped && s.slow >= MIRROR_SLOW {
            s.dropped = true;
            tracing::info!(mirror = %s.remote.as_ref().map_or("?", |r| r.url.as_str()), "dropping a slow mirror");
            return true;
        }
        false
    }

    /// Where requests go now, and the generation of that answer.
    fn remote(&self) -> (Remote, u64) {
        let remote = self.remote.read().unwrap_or_else(PoisonError::into_inner);
        (remote.clone(), self.generation.load(Ordering::Acquire))
    }

    /// A request to the current URL was refused with `code` (401/403/410). If that URL is a
    /// redirect target, resolves the original URL again — redirect targets are often signed
    /// links that expire while the original stays valid — and switches to the new target
    /// once it is shown to serve the same file. `seen` is the generation the failed request
    /// used: when another connection already switched, there is nothing to do.
    async fn relink(&self, seen: u64, code: u16, slot: u64) -> Result<()> {
        let _one_at_a_time = self.relinking.lock().await;
        if self.generation.load(Ordering::Acquire) != seen {
            return Ok(());
        }
        let (current, _) = self.remote();
        if current.url == self.original_url {
            return Err(Error::LinkExpired(code));
        }
        tracing::info!(expired = %current.url, "redirect target refused; resolving the original URL again");
        let (fresh, resp) = self.http.probe(&self.original_url, 0, None, slot).await?;
        drop(resp);
        let size = self.size.unwrap_or_default();
        let saved = self.table().saved();
        let sample = match crate::relink::sample_region(&saved) {
            Some((start, end)) => Some((
                start,
                self.file.read_at(start, (end - start) as usize).await?,
            )),
            None => None,
        };
        if fresh.url == current.url
            || !crate::relink::same_content(&self.http, &fresh, size, &sample, slot).await?
        {
            // The original leads back to the refused link, or to a different file.
            return Err(Error::LinkExpired(code));
        }
        tracing::info!(url = %fresh.url, "continuing with the new redirect target");
        let mut remote = self.remote.write().unwrap_or_else(PoisonError::into_inner);
        remote.url = fresh.url;
        remote.validators = fresh.validators;
        self.generation.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

impl Job<'_> {
    /// Returns the file size and how many bytes were resumed.
    async fn run(
        &self,
        progress: Option<&watch::Sender<Progress>>,
        cancel: &CancellationToken,
    ) -> Result<(u64, u64)> {
        let (mut remote, first_resp, table, resumed_from) = match self.try_resume().await? {
            Some(Resume::Complete(size)) => {
                self.size_known.send_replace(Some(Some(size)));
                return Ok((size, size));
            }
            Some(Resume::Partial(r)) => *r,
            None => {
                discard(self.part, self.state_path);
                let (remote, resp) = self.http.probe(&self.req.url, 0, None, 0).await?;
                let table = Table::new(remote.size.unwrap_or(u64::MAX));
                (remote, (0, resp), table, 0)
            }
        };
        self.size_known.send_replace(Some(remote.size));
        if !remote.ranges {
            self.opts.learn.note(|l| l.no_ranges = true);
        }
        let single = self.force_single || !remote.ranges || remote.size.is_none();
        if single {
            // Never send Range again; the probe body (from byte 0) is used as is.
            remote.ranges = false;
        }
        tracing::info!(url = %remote.url, size = ?remote.size, ranges = remote.ranges, resumed_from, "starting");

        if remote.size == Some(0) {
            drop(first_resp);
            std::fs::File::create(self.part)?;
            return Ok((0, 0));
        }

        if let Some(size) = remote.size {
            check_space(self.part, size.saturating_sub(resumed_from))?;
        }
        let mirrors = if single {
            Vec::new()
        } else {
            self.req.mirrors.clone()
        };
        let sources = Mutex::new((0..=mirrors.len()).map(|_| Source::default()).collect());
        let primary = remote.clone();
        let ctx = Arc::new(Ctx {
            sources,
            file: OutputFile::open(self.part, remote.size)?,
            http: self.http.clone(),
            size: remote.size,
            ranges: remote.ranges,
            remote: RwLock::new(remote),
            generation: AtomicU64::new(0),
            relinking: tokio::sync::Mutex::new(()),
            original_url: self.req.url.clone(),
            table: Mutex::new(table),
            read_timeout: self.opts.read_timeout,
            limiters: self.opts.limiters.clone(),
            shed: AtomicUsize::new(0),
            cancel: cancel.child_token(),
        });

        // Mirrors are checked while the download already runs from its own URL; each joins
        // as soon as it is shown to serve the same bytes.
        if !mirrors.is_empty() {
            let ctx = Arc::clone(&ctx);
            tokio::spawn(async move {
                tokio::select! {
                    () = check_mirrors(&ctx, &primary, &mirrors) => {}
                    () = ctx.cancel.cancelled() => {}
                }
            });
        }
        let mut workers: JoinSet<(usize, usize, Result<()>)> = JoinSet::new();
        let mut next_id = 1u64;
        // Per running segment: a stop switch, so a crawling connection can be dropped on its
        // own, and the connection slot it uses. Slots are recycled so a finished segment's
        // connection is reused by the next one; the probe used slot 0.
        let mut running: HashMap<usize, (CancellationToken, u64, usize)> = HashMap::new();
        let mut free_slots: Vec<u64> = Vec::new();
        let mut next_slot = 1u64;
        let spawn = |workers: &mut JoinSet<_>,
                     running: &mut HashMap<usize, (CancellationToken, u64, usize)>,
                     idx: usize,
                     slot: u64,
                     resp: Option<Response>| {
            let ctx = Arc::clone(&ctx);
            let stop = ctx.cancel.child_token();
            // The probe's response came from the download's own URL.
            let source = if resp.is_some() {
                ctx.sources()[0].active += 1;
                0
            } else {
                ctx.pick_source()
            };
            running.insert(idx, (stop.clone(), slot, source));
            workers.spawn(async move {
                let result = connection(&ctx, idx, source, slot, resp, &stop).await;
                (idx, source, result)
            });
        };

        let (idx, resp) = first_resp;
        ctx.table().claim(idx, next_id, Instant::now());
        next_id += 1;
        spawn(&mut workers, &mut running, idx, 0, Some(resp));

        let mut conn_limit = if single {
            1
        } else {
            self.opts.connections.max(1)
        };
        let mut fatal: Option<Error> = None;
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_tick = Instant::now();
        let mut last_checkpoint = Instant::now();
        let mut last_move = Instant::now();

        loop {
            tokio::select! {
                Some(joined) = workers.join_next(), if !workers.is_empty() => {
                    let (idx, source, result) = joined.map_err(|e| Error::State(format!("connection task: {e}")))?;
                    let now = Instant::now();
                    if let Some((_, slot, _)) = running.remove(&idx) {
                        free_slots.push(slot);
                    }
                    ctx.source_done(source, result.is_ok());
                    let mut t = ctx.table();
                    match result {
                        Ok(()) => t.release(idx, false, now),
                        Err(Error::Cancelled) => t.release(idx, false, now),
                        // A mirror's failure never stops the download: the range goes back
                        // to the table for another source, and the mirror is dropped when it
                        // refuses or keeps failing. Disk errors are the download's own.
                        Err(e) if source > 0 && !matches!(e, Error::Io(_) | Error::DiskFull { .. }) => {
                            tracing::debug!(segment = idx, source, "mirror request failed: {e}");
                            if ctx.source_failed(source, &e) {
                                stop_source(&running, source);
                            }
                            t.release(idx, false, now);
                        }
                        // Refused (401/403/410) while other connections are still downloading:
                        // the server limits connections and forbids the extra ones instead of
                        // saying it is busy. An expired link refuses every new request, so it
                        // still stops the download once no connection is left working.
                        Err(Error::LinkExpired(code)) if t.active() > 1 => {
                            let limit = t.active() - 1;
                            if limit < conn_limit {
                                tracing::info!("connection refused (HTTP {code}) while others work; limiting to {limit} connections");
                                conn_limit = limit;
                                self.opts.learn.limited_to(limit);
                            }
                            t.release(idx, true, now);
                        }
                        Err(e) if e.is_fatal() || matches!(e, Error::RangeIgnored) => {
                            t.release(idx, false, now);
                            fatal.get_or_insert(e);
                        }
                        Err(e) => {
                            tracing::debug!(segment = idx, "connection failed: {e}");
                            if let Error::Busy(code) = e {
                                let limit = t.active().saturating_sub(1).max(1);
                                if limit < conn_limit {
                                    tracing::info!("server busy (HTTP {code}); limiting to {limit} connections");
                                    conn_limit = limit;
                                    self.opts.learn.limited_to(limit);
                                }
                            }
                            t.release(idx, true, now);
                            if single {
                                t.rewind(idx);
                            }
                            if t.segs[idx].failures > self.opts.max_failures {
                                fatal.get_or_insert(Error::TooManyFailures {
                                    attempts: t.segs[idx].failures,
                                    last: e.to_string(),
                                });
                            }
                        }
                    }
                }
                _ = tick.tick() => {
                    let now = Instant::now();
                    let mut to_start = Vec::new();
                    {
                        let mut t = ctx.table();
                        let saturated_before = ctx.limiters.iter().any(Limiter::is_saturated);
                        t.sample_speeds(now - last_tick);
                        ctx.measure_sources(&t, &running);
                        // A mirror whose every connection has crawled for a while next to the
                        // best this download has seen is dropped as a whole: once the fast
                        // sources are done, its connections would only be compared with each
                        // other and keep the end of the file waiting.
                        if !saturated_before {
                            for source in slow_mirrors(&t, &running, now) {
                                if ctx.source_slow(source) {
                                    stop_source(&running, source);
                                }
                            }
                            // Connections on a source that gives a connection far less than the
                            // best one move: their ranges go back to the table and the new
                            // connections go to the best source. The speeds are measured again
                            // before the next moves, so a best source that slows down under
                            // the new load stops them.
                            if fatal.is_none() && now - last_move >= MOVE_EVERY {
                                let moving = ctx.connections_to_move(&t, &running, now);
                                for idx in &moving {
                                    if let Some((stop, _, _)) = running.get(idx) {
                                        tracing::debug!(segment = idx, "moving a connection to a faster source");
                                        stop.cancel();
                                    }
                                }
                                if !moving.is_empty() {
                                    last_move = now;
                                }
                            }
                        }
                        // A saturated limit sets the speed by itself: stealing would only waste
                        // what the victims' abandoned responses buffered. Crawlers are still
                        // replaced: that fixes the server, at the cost of one response.
                        // Under an unsaturated limit, only as many connections are added as the
                        // missing speed needs at the typical per-connection speed: every extra
                        // one throttled later fills socket buffers that a steal throws away.
                        let saturated = ctx.limiters.iter().any(Limiter::is_saturated);
                        let limit = ctx.tightest_limit();
                        let allowed = limit
                            .map_or(conn_limit, |r| {
                                conn_limit.min(usize::try_from(r / MIN_LIMITED_CONN_RATE).unwrap_or(usize::MAX).max(1))
                            })
                            .min(ctx.cap_while_checking(conn_limit));
                        // Lowering the limit below that while more run (a limit set during
                        // the download): the surplus closes between two requests, which costs
                        // nothing.
                        ctx.shed.store(t.active().saturating_sub(allowed), Ordering::Release);
                        // Connections started in the last tick or two have no speed yet; they
                        // are counted at the typical speed, or each tick would add more.
                        let unmeasured = t.unmeasured();
                        let mut budget = match (limit, t.typical_speed()) {
                            (Some(r), Some(per)) => {
                                let missing = r as f64 - t.total_speed() - unmeasured as f64 * per;
                                let n = (missing / per).ceil().max(0.0) as usize;
                                if unmeasured == 0 { n.max(1) } else { n }
                            }
                            (Some(_), None) => usize::from(unmeasured == 0),
                            (None, _) => usize::MAX,
                        };
                        if fatal.is_none() {
                            while t.active() < allowed && budget > 0 {
                                budget -= 1;
                                match t.assign(next_id, now, &self.opts.split, !saturated) {
                                    Some(idx) => {
                                        next_id += 1;
                                        to_start.push(idx);
                                    }
                                    None => break,
                                }
                            }
                        }
                        // Under a saturated limit a slow connection is slowed by the limit,
                        // not the server: dropping it gains nothing and throws away what
                        // it buffered.
                        if !single
                            && !saturated
                            && fatal.is_none()
                            && let Some(idx) = t.laggard(now)
                            && let Some((stop, _, source)) = running.get(&idx).filter(|(s, _, _)| !s.is_cancelled())
                        {
                            t.segs[idx].evictions += 1;
                            if ctx.source_slow(*source) {
                                stop_source(&running, *source);
                            }
                            tracing::debug!(segment = idx, speed = t.segs[idx].speed, "dropping crawling connection");
                            stop.cancel();
                        }
                        let active = t.active();
                        self.opts.learn.note(|l| l.peak_connections = l.peak_connections.max(active));
                        if let Some(p) = progress {
                            p.send_replace(snapshot(&t, ctx.size));
                        }
                    }
                    last_tick = now;
                    for idx in to_start {
                        let slot = free_slots.pop().unwrap_or_else(|| {
                            next_slot += 1;
                            next_slot - 1
                        });
                        spawn(&mut workers, &mut running, idx, slot, None);
                    }
                    if fatal.is_none() && now - last_checkpoint >= self.opts.checkpoint_every {
                        self.checkpoint(&ctx, false).await?;
                        last_checkpoint = now;
                    }
                }
                () = cancel.cancelled(), if fatal.is_none() => {
                    fatal = Some(Error::Cancelled);
                }
            }

            if fatal.is_some() {
                ctx.cancel.cancel();
                if workers.is_empty() {
                    break;
                }
            } else if workers.is_empty() && ctx.table().is_complete() {
                break;
            }
        }

        if let Some(e) = fatal {
            if !matches!(e, Error::FileChanged | Error::RangeIgnored) {
                self.checkpoint(&ctx, true).await.ok();
            }
            return Err(e);
        }

        let received = ctx.table().received_total();
        if ctx.size.is_none() {
            ctx.file.set_len(received)?;
        }
        if let Some(p) = progress {
            p.send_replace(snapshot(&ctx.table(), Some(received)));
        }
        // Every connection task has finished, so this is the last handle to the file and it
        // is closed here, before the caller renames it.
        drop(ctx);
        Ok((received, resumed_from))
    }

    /// Picks up a previous attempt if its state file matches the server's current file.
    async fn try_resume(&self) -> Result<Option<Resume>> {
        if self.force_single || !self.part.exists() {
            return Ok(None);
        }
        let Some(state) = State::load(self.state_path) else {
            return Ok(None);
        };
        let table = Table::from_saved(&state.segments);
        if state.url != self.req.url {
            return self.adopt_link(&state, table).await;
        }
        let Some(idx) = table.segs.iter().position(|s| !s.is_done()) else {
            // Every byte was already on disk when the last attempt stopped.
            return Ok(Some(Resume::Complete(state.size)));
        };
        let start = table.segs[idx].written;
        let (remote, resp) = self
            .http
            .probe(&self.req.url, start, state.validators.if_range(), 0)
            .await?;
        if remote.ranges
            && remote.size == Some(state.size)
            && state.validators.same_file(&remote.validators)
        {
            let resumed = table.received_total();
            tracing::info!(resumed, "resuming");
            Ok(Some(Resume::Partial(Box::new((
                remote,
                (idx, resp),
                table,
                resumed,
            )))))
        } else {
            tracing::info!("remote file changed or lost range support; starting over");
            Ok(None)
        }
    }

    /// The interrupted attempt used another URL: continue with this one if it serves the same
    /// file (a refreshed link), otherwise start over.
    async fn adopt_link(&self, state: &State, table: Table) -> Result<Option<Resume>> {
        let idx = table.segs.iter().position(|s| !s.is_done());
        let start = idx.map_or(0, |i| table.segs[i].written);
        let (remote, resp) = self.http.probe(&self.req.url, start, None, 0).await?;
        let sample = crate::relink::read_sample(self.part, &state.segments)?;
        if !crate::relink::same_content(self.http, &remote, state.size, &sample, 1).await? {
            tracing::info!("the new link serves a different file; starting over");
            return Ok(None);
        }
        let Some(idx) = idx else {
            return Ok(Some(Resume::Complete(state.size)));
        };
        let resumed = table.received_total();
        tracing::info!(
            resumed,
            "continuing the interrupted download with the new link"
        );
        Ok(Some(Resume::Partial(Box::new((
            remote,
            (idx, resp),
            table,
            resumed,
        )))))
    }

    /// Saves progress. Positions are captured before the state is written, so the state
    /// never claims bytes that were not yet handed to the OS: that survives the process
    /// crashing or being killed. With `durable`, the file is also flushed to the disk first,
    /// which additionally survives power loss — but on Windows a flush stalls every write
    /// to the file for seconds, so it is only done when stopping (see ADR 0002).
    async fn checkpoint(&self, ctx: &Ctx, durable: bool) -> Result<()> {
        let Some(size) = ctx.size.filter(|_| ctx.ranges) else {
            return Ok(()); // without ranges there is nothing to resume from
        };
        let segments = ctx.table().saved();
        if durable {
            ctx.file.sync().await?;
        }
        State::new(&self.req.url, size, ctx.remote().0.validators, segments)
            .save(self.state_path)?;
        Ok(())
    }
}

/// Fails with [`Error::DiskFull`] when the disk holding `part` has less than `need` bytes
/// free. The file is sparse, so space is only taken as data arrives and a full disk would
/// otherwise show up much later, in the middle of the download.
fn check_space(part: &Path, need: u64) -> Result<()> {
    let dir = match part.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    match fs4::available_space(dir) {
        Ok(free) if free < need => Err(Error::DiskFull {
            need: Some(need),
            free: Some(free),
        }),
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::debug!("could not read free space: {e}");
            Ok(())
        }
    }
}

enum Resume {
    /// All data is already on disk.
    Complete(u64),
    /// Continue: remote info, first segment to download with its open response, the
    /// restored table, bytes already on disk.
    Partial(Box<(Remote, (usize, Response), Table, u64)>),
}

fn snapshot(t: &Table, size: Option<u64>) -> Progress {
    Progress {
        phase: Phase::Downloading,
        size,
        received: t.received_total(),
        speed: t.total_speed(),
        connections: t.active(),
        segments: t
            .segs
            .iter()
            .map(|s| SegmentView {
                start: s.start,
                end: s.end,
                received: s.received,
                active: s.owner.is_some(),
                speed: s.speed,
            })
            .collect(),
        ..Progress::default()
    }
}

/// One connection: downloads its segment until it reaches the segment's end (which may
/// move closer while it runs, when another connection steals the rest), buffering data
/// into large positional writes.
async fn connection(
    ctx: &Ctx,
    idx: usize,
    source: usize,
    slot: u64,
    initial: Option<Response>,
    stop: &CancellationToken,
) -> Result<()> {
    let start = ctx.table().segs[idx].received;
    let mut w = Writer {
        buf: Vec::with_capacity(WRITE_CHUNK),
        offset: start,
        last: Instant::now(),
    };
    let mut pos = start;
    let mut initial = initial;
    let result = loop {
        let end = ctx.table().segs[idx].end;
        if pos >= end {
            break Ok(());
        }
        // The first response may cover more than the segment (the probe asks for the rest
        // of the file); later ones are asked for exactly.
        let (resp, asked_end) = match initial.take() {
            Some(r) => (r, u64::MAX),
            None => {
                if pos > start && ctx.take_shed() {
                    // Asked to close: the rest of the segment goes back to the table.
                    break Ok(());
                }
                let asked_end = ctx.window_end(pos, end);
                match request(ctx, source, pos, asked_end, slot, stop).await {
                    Ok(r) => (r, asked_end),
                    Err(e) => break Err(e),
                }
            }
        };
        if let Err(e) = receive(ctx, idx, resp, &mut pos, asked_end, &mut w, stop).await {
            break Err(e);
        }
    };
    // Keep whatever arrived before a failure.
    let flushed = w.flush(ctx, idx).await;
    result.and(flushed)
}

/// Checks each mirror against `primary`: the same size, ranges, and the same first bytes.
/// One that passes is put in use; the rest are left out (logged), never failing the
/// download.
async fn check_mirrors(ctx: &Ctx, primary: &Remote, urls: &[String]) {
    let size = primary.size.unwrap_or_default();
    // Slots of their own, apart from the connections'.
    const SLOT: u64 = 1 << 32;
    let sample = match ctx
        .http
        .fetch(primary, 0, size.min(MIRROR_SAMPLE), SLOT)
        .await
    {
        Ok(data) => data,
        Err(e) => {
            tracing::info!("mirrors not used: could not read the start of the file: {e}");
            for s in ctx.sources().iter_mut().skip(1) {
                s.dropped = true;
            }
            return;
        }
    };
    let checks = urls.iter().enumerate().map(|(i, url)| {
        let sample = &sample;
        async move {
            let slot = SLOT + 1 + i as u64;
            // Asks for the sample only, so a mirror of another file sends little.
            let result = tokio::time::timeout(
                MIRROR_CHECK,
                ctx.http.probe_sample(url, sample.len() as u64, slot),
            )
            .await
            .unwrap_or_else(|_| Err(Error::Stalled(MIRROR_CHECK)));
            match result {
                Ok((remote, head))
                    if remote.ranges
                        && remote.size == Some(size)
                        && head.as_deref() == Some(sample.as_slice()) =>
                {
                    tracing::info!(mirror = %url, "mirror in use");
                    ctx.sources()[i + 1].remote = Some(remote);
                }
                Ok(_) => {
                    tracing::info!(mirror = %url, "mirror left out: it serves another file");
                    ctx.sources()[i + 1].dropped = true;
                }
                Err(e) => {
                    tracing::info!(mirror = %url, "mirror left out: {e}");
                    ctx.sources()[i + 1].dropped = true;
                }
            }
        }
    });
    futures_util::future::join_all(checks).await;
}

/// Mirrors whose connections, all at least [`SETTLED`] old, have stayed below
/// [`SLOW_MIRROR`] of the best connection speed seen.
fn slow_mirrors(
    t: &Table,
    running: &HashMap<usize, (CancellationToken, u64, usize)>,
    now: Instant,
) -> Vec<usize> {
    let best = t.best_speed();
    if best <= 0.0 {
        return Vec::new();
    }
    // Per mirror: (connections, connections settled and slow).
    let mut seen: HashMap<usize, (usize, usize)> = HashMap::new();
    for (&idx, &(_, _, source)) in running {
        if source == 0 {
            continue;
        }
        let s = &t.segs[idx];
        let settled = s.owned_since.is_some_and(|at| now - at >= SETTLED);
        let entry = seen.entry(source).or_default();
        entry.0 += 1;
        if settled && s.speed < SLOW_MIRROR * best {
            entry.1 += 1;
        }
    }
    seen.into_iter()
        .filter(|(_, (all, slow))| *all > 0 && slow == all)
        .map(|(source, _)| source)
        .collect()
}

/// Stops every connection still using a source that was just dropped; their ranges go back
/// to the table for the other sources.
fn stop_source(running: &HashMap<usize, (CancellationToken, u64, usize)>, source: usize) {
    for (stop, _, s) in running.values() {
        if *s == source {
            stop.cancel();
        }
    }
}

/// Asks `source` for `start..end` of the file (`end` = `u64::MAX`: to the end). The
/// download's own URL is resolved again once if it has expired; a mirror is not.
async fn request(
    ctx: &Ctx,
    source: usize,
    start: u64,
    end: u64,
    slot: u64,
    stop: &CancellationToken,
) -> Result<Response> {
    let end = (end != u64::MAX).then_some(end);
    if source > 0 {
        let Some(mirror) = ctx.sources()[source].remote.clone() else {
            return Err(Error::State("mirror not checked".into()));
        };
        return tokio::select! {
            r = ctx.http.range(&mirror, start, end, slot) => r,
            () = stop.cancelled() => Err(Error::Cancelled),
        };
    }
    let mut relinked = false;
    loop {
        let (remote, generation) = ctx.remote();
        let r = tokio::select! {
            r = ctx.http.range(&remote, start, end, slot) => r,
            () = stop.cancelled() => return Err(Error::Cancelled),
        };
        match r {
            Err(Error::LinkExpired(code)) if !relinked => {
                ctx.relink(generation, code, slot).await?;
                relinked = true;
            }
            r => return r,
        }
    }
}

/// Reads one response into segment `idx` until the segment's end (which may move closer
/// while it runs, when another connection steals the rest) or the end of what was asked
/// for (`asked_end`), whichever comes first.
async fn receive(
    ctx: &Ctx,
    idx: usize,
    resp: Response,
    pos: &mut u64,
    asked_end: u64,
    w: &mut Writer,
    stop: &CancellationToken,
) -> Result<()> {
    let mut body = Box::pin(resp.bytes_stream());
    loop {
        let chunk = tokio::select! {
            () = stop.cancelled() => break Err(Error::Cancelled),
            r = tokio::time::timeout(ctx.read_timeout, body.next()) => match r {
                Err(_) => break Err(Error::Stalled(ctx.read_timeout)),
                Ok(Some(Err(e))) => break Err(e.into()),
                Ok(Some(Ok(c))) => Some(c),
                Ok(None) => None,
            },
        };
        let Some(chunk) = chunk else {
            let mut t = ctx.table();
            let seg = &mut t.segs[idx];
            if seg.end == u64::MAX {
                seg.end = *pos; // size was unknown: the stream's end is the file's end
                return Ok(());
            }
            return if *pos >= seg.end.min(asked_end) {
                Ok(())
            } else {
                Err(Error::Truncated)
            };
        };
        // Clip to the current end; it may have shrunk since the request was made.
        let (take, until) = {
            let mut t = ctx.table();
            let seg = &mut t.segs[idx];
            let until = seg.end.min(asked_end);
            let take = (until.saturating_sub(*pos)).min(chunk.len() as u64) as usize;
            seg.received = *pos + take as u64;
            (take, until)
        };
        w.buf.extend_from_slice(&chunk[..take]);
        *pos += take as u64;
        let finished = *pos >= until;
        // A request made before a limit was set (or lowered) asks for far more than a window
        // now: it is closed and asked again in windows.
        let rewindow =
            !finished && asked_end.saturating_sub(ctx.window_end(*pos, u64::MAX)) > MIN_WINDOW;
        if finished && *pos == asked_end {
            // Everything asked for arrived: reading the end of the body lets the connection
            // be reused for the next request.
            let _ = tokio::time::timeout(Duration::from_millis(100), body.next()).await;
        }
        if finished || rewindow {
            // Closed before waiting for the limit, so the buffers stop filling: whatever
            // the server sent past this point is lost.
            drop(body);
            pay(ctx, take, stop).await;
            w.flush(ctx, idx).await?;
            return Ok(());
        }
        pay(ctx, take, stop).await;
        if w.buf.len() >= WRITE_CHUNK || w.last.elapsed() >= WRITE_EVERY {
            w.flush(ctx, idx).await?;
        }
    }
}

/// Waits until every speed limit has paid for `bytes` just received.
async fn pay(ctx: &Ctx, bytes: usize, stop: &CancellationToken) {
    for limiter in &ctx.limiters {
        tokio::select! {
            () = stop.cancelled() => return,
            () = limiter.acquire(bytes) => {}
        }
    }
}

struct Writer {
    buf: Vec<u8>,
    offset: u64,
    last: Instant,
}

impl Writer {
    async fn flush(&mut self, ctx: &Ctx, idx: usize) -> Result<()> {
        self.last = Instant::now();
        if self.buf.is_empty() {
            return Ok(());
        }
        let len = self.buf.len() as u64;
        let data = std::mem::take(&mut self.buf);
        let mut data = ctx.file.write_at(data, self.offset).await?;
        self.offset += len;
        ctx.table().segs[idx].written = self.offset;
        data.clear();
        self.buf = data;
        Ok(())
    }
}
