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
/// Bounds of a request's size under a speed limit (see [`Ctx::window_end`]).
const MIN_WINDOW: u64 = 256 << 10;
const MAX_WINDOW: u64 = 16 << 20;

pub struct Request {
    pub url: String,
    /// Final file path. While downloading, data goes to `<output>.idma-part`, resume
    /// information to `<output>.idma-state`, and `<output>.idma-lock` keeps a second
    /// download of the same file away. See [`crate::target`] for choosing a free path.
    pub output: PathBuf,
    pub headers: Vec<(String, String)>,
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
    Downloading,
    /// Hashing the finished file; see [`Progress::verified`].
    Verifying,
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
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub size: u64,
    /// Bytes already on disk from an earlier attempt.
    pub resumed_from: u64,
    pub elapsed: Duration,
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
    let part = sibling(&req.output, ".idma-part");
    let state_path = sibling(&req.output, ".idma-state");
    let mut force_single = false;
    loop {
        let job = Job {
            req,
            opts,
            http: &http,
            part: &part,
            state_path: &state_path,
            force_single,
        };
        match job.run(progress, cancel).await {
            Ok((size, resumed_from)) => {
                if let Some(want) = &opts.checksum {
                    match verify(&part, want, progress, cancel).await {
                        // Interrupted: the complete part and its state stay, so the next
                        // attempt only verifies again.
                        Err(Error::Cancelled) => return Err(Error::Cancelled),
                        // Keep the file for inspection; downloading again starts over.
                        Err(mismatch @ Error::ChecksumMismatch { .. }) => {
                            std::fs::rename(&part, &req.output)?;
                            std::fs::remove_file(&state_path).ok();
                            return Err(mismatch);
                        }
                        other => other?,
                    }
                }
                std::fs::rename(&part, &req.output)?;
                std::fs::remove_file(&state_path).ok();
                if let Some(p) = progress {
                    p.send_modify(|p| p.phase = Phase::Done);
                }
                return Ok(Outcome {
                    size,
                    resumed_from,
                    elapsed: started.elapsed(),
                });
            }
            Err(Error::RangeIgnored) if !force_single => {
                tracing::warn!("server ignores range requests; restarting with one connection");
                discard(&part, &state_path);
                force_single = true;
            }
            Err(Error::FileChanged) => {
                discard(&part, &state_path);
                return Err(Error::FileChanged);
            }
            Err(e) => return Err(e),
        }
    }
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
    cancel: CancellationToken,
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
            Some(Resume::Complete(size)) => return Ok((size, size)),
            Some(Resume::Partial(r)) => *r,
            None => {
                discard(self.part, self.state_path);
                let (remote, resp) = self.http.probe(&self.req.url, 0, None, 0).await?;
                let table = Table::new(remote.size.unwrap_or(u64::MAX));
                (remote, (0, resp), table, 0)
            }
        };
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
        let ctx = Arc::new(Ctx {
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

        let mut workers: JoinSet<(usize, Result<()>)> = JoinSet::new();
        let mut next_id = 1u64;
        // Per running segment: a stop switch, so a crawling connection can be dropped on its
        // own, and the connection slot it uses. Slots are recycled so a finished segment's
        // connection is reused by the next one; the probe used slot 0.
        let mut running: HashMap<usize, (CancellationToken, u64)> = HashMap::new();
        let mut free_slots: Vec<u64> = Vec::new();
        let mut next_slot = 1u64;
        let spawn = |workers: &mut JoinSet<_>,
                     running: &mut HashMap<usize, (CancellationToken, u64)>,
                     idx: usize,
                     slot: u64,
                     resp: Option<Response>| {
            let ctx = Arc::clone(&ctx);
            let stop = ctx.cancel.child_token();
            running.insert(idx, (stop.clone(), slot));
            workers.spawn(async move { (idx, connection(&ctx, idx, slot, resp, &stop).await) });
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

        loop {
            tokio::select! {
                Some(joined) = workers.join_next(), if !workers.is_empty() => {
                    let (idx, result) = joined.map_err(|e| Error::State(format!("connection task: {e}")))?;
                    let now = Instant::now();
                    if let Some((_, slot)) = running.remove(&idx) {
                        free_slots.push(slot);
                    }
                    let mut t = ctx.table();
                    match result {
                        Ok(()) => t.release(idx, false, now),
                        Err(Error::Cancelled) => t.release(idx, false, now),
                        // Refused (401/403/410) while other connections are still downloading:
                        // the server limits connections and forbids the extra ones instead of
                        // saying it is busy. An expired link refuses every new request, so it
                        // still stops the download once no connection is left working.
                        Err(Error::LinkExpired(code)) if t.active() > 1 => {
                            let limit = t.active() - 1;
                            if limit < conn_limit {
                                tracing::info!("connection refused (HTTP {code}) while others work; limiting to {limit} connections");
                                conn_limit = limit;
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
                        t.sample_speeds(now - last_tick);
                        // A saturated limit sets the speed by itself: stealing would only waste
                        // what the victims' abandoned responses buffered. Crawlers are still
                        // replaced: that fixes the server, at the cost of one response.
                        // Under an unsaturated limit, only as many connections are added as the
                        // missing speed needs at the typical per-connection speed: every extra
                        // one throttled later fills socket buffers that a steal throws away.
                        let saturated = ctx.limiters.iter().any(Limiter::is_saturated);
                        let limit = ctx.tightest_limit();
                        let allowed = limit.map_or(conn_limit, |r| {
                            conn_limit.min(usize::try_from(r / MIN_LIMITED_CONN_RATE).unwrap_or(usize::MAX).max(1))
                        });
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
                            && let Some((stop, _)) = running.get(&idx).filter(|(s, _)| !s.is_cancelled())
                        {
                            t.segs[idx].evictions += 1;
                            tracing::debug!(segment = idx, speed = t.segs[idx].speed, "dropping crawling connection");
                            stop.cancel();
                        }
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
        verified: 0,
    }
}

/// One connection: downloads its segment until it reaches the segment's end (which may
/// move closer while it runs, when another connection steals the rest), buffering data
/// into large positional writes.
async fn connection(
    ctx: &Ctx,
    idx: usize,
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
                match request(ctx, pos, asked_end, slot, stop).await {
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

/// Asks for `start..end` of the file (`end` = `u64::MAX`: to the end), resolving the
/// original URL again once if the current one has expired.
async fn request(
    ctx: &Ctx,
    start: u64,
    end: u64,
    slot: u64,
    stop: &CancellationToken,
) -> Result<Response> {
    let end = (end != u64::MAX).then_some(end);
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
