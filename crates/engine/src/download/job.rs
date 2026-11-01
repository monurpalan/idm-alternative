//! A download's main loop: connections started and steered, resuming a previous attempt
//! (or adopting a new link for it), checkpoints, and the free-space check.

use super::*;

pub(crate) struct Job<'a> {
    pub(crate) req: &'a Request,
    pub(crate) opts: &'a Options,
    pub(crate) http: &'a Http,
    pub(crate) part: &'a Path,
    pub(crate) state_path: &'a Path,
    pub(crate) force_single: bool,
    /// Told the file's size (`Some(None)`: unknown) once the server has said.
    pub(crate) size_known: &'a watch::Sender<Option<Option<u64>>>,
}

impl Job<'_> {
    /// Returns the file size and how many bytes were resumed.
    pub(crate) async fn run(
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
    pub(crate) async fn try_resume(&self) -> Result<Option<Resume>> {
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
    pub(crate) async fn adopt_link(&self, state: &State, table: Table) -> Result<Option<Resume>> {
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
    pub(crate) async fn checkpoint(&self, ctx: &Ctx, durable: bool) -> Result<()> {
        let Some(size) = ctx.size.filter(|_| ctx.ranges) else {
            return Ok(()); // without ranges there is nothing to resume from
        };
        let segments = ctx.table().saved();
        if durable {
            ctx.file.sync().await?;
        }
        let mut state = State::new(&self.req.url, size, ctx.remote().0.validators, segments);
        state.clean = durable;
        state.save(self.state_path)?;
        Ok(())
    }
}

/// Fails with [`Error::DiskFull`] when the disk holding `part` has less than `need` bytes
/// free. The file is sparse, so space is only taken as data arrives and a full disk would
/// otherwise show up much later, in the middle of the download.
pub(crate) fn check_space(part: &Path, need: u64) -> Result<()> {
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

pub(crate) enum Resume {
    /// All data is already on disk.
    Complete(u64),
    /// Continue: remote info, first segment to download with its open response, the
    /// restored table, bytes already on disk.
    Partial(Box<(Remote, (usize, Response), Table, u64)>),
}

pub(crate) fn snapshot(t: &Table, size: Option<u64>) -> Progress {
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
