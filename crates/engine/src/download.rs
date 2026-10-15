//! One download: probe, resume or start fresh, run connections, checkpoint, finish.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use reqwest::Response;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};
use crate::file::OutputFile;
use crate::remote::{Http, Remote};
use crate::segments::{SplitPolicy, Table};
use crate::state::{State, sibling};

/// Bytes a connection collects before handing them to the OS in one write.
const WRITE_CHUNK: usize = 1 << 20;
/// A connection's buffer is written at least this often, so checkpoints keep moving.
const WRITE_EVERY: Duration = Duration::from_secs(1);
const TICK: Duration = Duration::from_millis(100);

pub struct Request {
    pub url: String,
    /// Final file path. While downloading, data goes to `<output>.idma-part` and resume
    /// information to `<output>.idma-state`.
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
    pub user_agent: String,
    pub split: SplitPolicy,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            connections: 16,
            read_timeout: Duration::from_secs(20),
            checkpoint_every: Duration::from_secs(2),
            max_failures: 8,
            user_agent: format!("idm-alternative/{}", crate::VERSION),
            split: SplitPolicy::default(),
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

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub size: Option<u64>,
    pub received: u64,
    /// Bytes per second over the last moments.
    pub speed: f64,
    pub connections: usize,
    pub segments: Vec<SegmentView>,
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
    let http = Http::new(&opts.user_agent, &req.headers, opts.connections)?;
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
                std::fs::rename(&part, &req.output)?;
                std::fs::remove_file(&state_path).ok();
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
    remote: Remote,
    file: OutputFile,
    table: Mutex<Table>,
    read_timeout: Duration,
    cancel: CancellationToken,
}

impl Ctx {
    fn table(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
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
                let (remote, resp) = self.http.probe(&self.req.url, 0, None).await?;
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

        let ctx = Arc::new(Ctx {
            file: OutputFile::open(self.part, remote.size)?,
            http: self.http.clone(),
            remote,
            table: Mutex::new(table),
            read_timeout: self.opts.read_timeout,
            cancel: cancel.child_token(),
        });

        let mut workers: JoinSet<(usize, Result<()>)> = JoinSet::new();
        let mut next_id = 1u64;
        // Per-connection stop switch, so a crawling connection can be dropped on its own.
        let mut stops: HashMap<usize, CancellationToken> = HashMap::new();
        let spawn = |workers: &mut JoinSet<_>,
                     stops: &mut HashMap<usize, CancellationToken>,
                     idx: usize,
                     resp: Option<Response>| {
            let ctx = Arc::clone(&ctx);
            let stop = ctx.cancel.child_token();
            stops.insert(idx, stop.clone());
            workers.spawn(async move { (idx, connection(&ctx, idx, resp, &stop).await) });
        };

        let (idx, resp) = first_resp;
        ctx.table().claim(idx, next_id, Instant::now());
        next_id += 1;
        spawn(&mut workers, &mut stops, idx, Some(resp));

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
                    stops.remove(&idx);
                    let mut t = ctx.table();
                    match result {
                        Ok(()) => t.release(idx, false, now),
                        Err(Error::Cancelled) => t.release(idx, false, now),
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
                        if fatal.is_none() {
                            while t.active() < conn_limit {
                                match t.assign(next_id, now, &self.opts.split) {
                                    Some(idx) => {
                                        next_id += 1;
                                        to_start.push(idx);
                                    }
                                    None => break,
                                }
                            }
                        }
                        if !single
                            && fatal.is_none()
                            && let Some(idx) = t.laggard(now)
                            && let Some(stop) = stops.get(&idx).filter(|s| !s.is_cancelled())
                        {
                            t.segs[idx].evictions += 1;
                            tracing::debug!(segment = idx, speed = t.segs[idx].speed, "dropping crawling connection");
                            stop.cancel();
                        }
                        if let Some(p) = progress {
                            p.send_replace(snapshot(&t, ctx.remote.size));
                        }
                    }
                    last_tick = now;
                    for idx in to_start {
                        spawn(&mut workers, &mut stops, idx, None);
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
        if ctx.remote.size.is_none() {
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
        let Some(state) = State::load(self.state_path).filter(|s| s.url == self.req.url) else {
            return Ok(None);
        };
        let table = Table::from_saved(&state.segments);
        let Some(idx) = table.segs.iter().position(|s| !s.is_done()) else {
            // Every byte was already on disk when the last attempt stopped.
            return Ok(Some(Resume::Complete(state.size)));
        };
        let start = table.segs[idx].written;
        let (remote, resp) = self
            .http
            .probe(&self.req.url, start, state.validators.if_range())
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

    /// Saves progress. Positions are captured before the state is written, so the state
    /// never claims bytes that were not yet handed to the OS: that survives the process
    /// crashing or being killed. With `durable`, the file is also flushed to the disk first,
    /// which additionally survives power loss — but on Windows a flush stalls every write
    /// to the file for seconds, so it is only done when stopping (see ADR 0002).
    async fn checkpoint(&self, ctx: &Ctx, durable: bool) -> Result<()> {
        let Some(size) = ctx.remote.size.filter(|_| ctx.remote.ranges) else {
            return Ok(()); // without ranges there is nothing to resume from
        };
        let segments = ctx.table().saved();
        if durable {
            ctx.file.sync().await?;
        }
        State::new(&self.req.url, size, ctx.remote.validators.clone(), segments)
            .save(self.state_path)?;
        Ok(())
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
    }
}

/// One connection: downloads its segment until it reaches the segment's end (which may
/// move closer while it runs, when another connection steals the rest), buffering data
/// into large positional writes.
async fn connection(
    ctx: &Ctx,
    idx: usize,
    initial: Option<Response>,
    stop: &CancellationToken,
) -> Result<()> {
    let (start, end) = {
        let t = ctx.table();
        (t.segs[idx].received, t.segs[idx].end)
    };
    if start >= end {
        return Ok(());
    }
    let mut resp = match initial {
        Some(r) => r,
        None => {
            let end = (end != u64::MAX).then_some(end);
            tokio::select! {
                r = ctx.http.range(&ctx.remote, start, end) => r?,
                () = stop.cancelled() => return Err(Error::Cancelled),
            }
        }
    };

    let mut w = Writer {
        buf: Vec::with_capacity(WRITE_CHUNK),
        offset: start,
        last: Instant::now(),
    };
    let mut pos = start;
    let result = loop {
        let chunk = tokio::select! {
            () = stop.cancelled() => break Err(Error::Cancelled),
            r = tokio::time::timeout(ctx.read_timeout, resp.chunk()) => match r {
                Err(_) => break Err(Error::Stalled(ctx.read_timeout)),
                Ok(Err(e)) => break Err(e.into()),
                Ok(Ok(c)) => c,
            },
        };
        let Some(chunk) = chunk else {
            let mut t = ctx.table();
            let seg = &mut t.segs[idx];
            if seg.end == u64::MAX {
                seg.end = pos; // size was unknown: the stream's end is the file's end
                break Ok(());
            }
            break if pos >= seg.end {
                Ok(())
            } else {
                Err(Error::Truncated)
            };
        };
        // Clip to the current end; it may have shrunk since the request was made.
        let (take, seg_end) = {
            let mut t = ctx.table();
            let seg = &mut t.segs[idx];
            let take = (seg.end.saturating_sub(pos)).min(chunk.len() as u64) as usize;
            seg.received = pos + take as u64;
            (take, seg.end)
        };
        w.buf.extend_from_slice(&chunk[..take]);
        pos += take as u64;
        let finished = pos >= seg_end;
        if (finished || w.buf.len() >= WRITE_CHUNK || w.last.elapsed() >= WRITE_EVERY)
            && let Err(e) = w.flush(ctx, idx).await
        {
            break Err(e);
        }
        if finished {
            // Dropping the response abandons the rest of an open-ended body.
            break Ok(());
        }
    };
    // Keep whatever arrived before a failure.
    let flushed = w.flush(ctx, idx).await;
    result.and(flushed)
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
