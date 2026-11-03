//! What a download's main loop (see `job.rs`) does when a connection ends and on every tick:
//! errors acted on, sources and connections steered, connections added and dropped.

use super::*;

/// The state of a download's main loop.
pub(crate) struct Steer<'a> {
    pub(crate) job: &'a Job<'a>,
    pub(crate) ctx: Arc<Ctx>,
    pub(crate) single: bool,
    pub(crate) workers: JoinSet<(usize, usize, Result<()>)>,
    /// Per running segment: a stop switch, so a crawling connection can be dropped on its
    /// own, the connection slot it uses and its source. Slots are recycled so a finished
    /// segment's connection is reused by the next one; the probe used slot 0.
    pub(crate) running: HashMap<usize, (CancellationToken, u64, usize)>,
    pub(crate) free_slots: Vec<u64>,
    pub(crate) next_slot: u64,
    pub(crate) next_id: u64,
    pub(crate) conn_limit: usize,
    pub(crate) fatal: Option<Error>,
    pub(crate) last_tick: Instant,
    pub(crate) last_checkpoint: Instant,
    pub(crate) last_move: Instant,
    pub(crate) health: Health,
    pub(crate) disk: DiskGauge,
}

impl Steer<'_> {
    /// Starts a connection for segment `idx` on connection slot `slot`; `resp` is the probe's
    /// response, which came from the download's own URL.
    pub(crate) fn spawn(&mut self, idx: usize, slot: u64, resp: Option<Response>) {
        let ctx = Arc::clone(&self.ctx);
        let stop = ctx.cancel.child_token();
        // The probe's response came from the download's own URL.
        let source = if resp.is_some() {
            ctx.sources()[0].active += 1;
            0
        } else {
            ctx.pick_source()
        };
        self.running.insert(idx, (stop.clone(), slot, source));
        self.workers.spawn(async move {
            let result = connection(&ctx, idx, source, slot, resp, &stop).await;
            (idx, source, result)
        });
    }

    /// Runs the connections until the file is complete, or, with [`Steer::fatal`] set, until
    /// every connection has stopped.
    pub(crate) async fn drive(
        &mut self,
        progress: Option<&watch::Sender<Progress>>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                Some(joined) = self.workers.join_next(), if !self.workers.is_empty() => {
                    let (idx, source, result) = joined.map_err(|e| Error::State(format!("connection task: {e}")))?;
                    self.joined(idx, source, result);
                }
                _ = tick.tick() => self.tick(progress).await?,
                () = cancel.cancelled(), if self.fatal.is_none() => {
                    self.fatal = Some(Error::Cancelled);
                }
            }

            if self.fatal.is_some() {
                self.ctx.cancel.cancel();
                if self.workers.is_empty() {
                    break;
                }
            } else if self.workers.is_empty() && self.ctx.table().is_complete() {
                break;
            }
        }
        Ok(())
    }

    /// A connection ended: its segment goes back to the table, and its error is acted on.
    pub(crate) fn joined(&mut self, idx: usize, source: usize, result: Result<()>) {
        let now = Instant::now();
        if let Some((_, slot, _)) = self.running.remove(&idx) {
            self.free_slots.push(slot);
        }
        self.ctx.source_done(source, result.is_ok());
        let ctx = Arc::clone(&self.ctx);
        let mut t = ctx.table();
        match result {
            Ok(()) => t.release(idx, false, now),
            Err(Error::Cancelled) => t.release(idx, false, now),
            // A mirror's failure never stops the download: the range goes back
            // to the table for another source, and the mirror is dropped when it
            // refuses or keeps failing. Disk errors are the download's own.
            Err(e) if source > 0 && !matches!(e, Error::Io(_) | Error::DiskFull { .. }) => {
                tracing::debug!(segment = idx, source, "mirror request failed: {e}");
                if self.ctx.source_failed(source, &e) {
                    stop_source(&self.running, source);
                }
                t.release(idx, false, now);
            }
            // Refused (401/403/410) while other connections are still downloading:
            // the server limits connections and forbids the extra ones instead of
            // saying it is busy. An expired link refuses every new request, so it
            // still stops the download once no connection is left working.
            Err(Error::LinkExpired(code)) if t.active() > 1 => {
                let limit = t.active() - 1;
                if limit < self.conn_limit {
                    tracing::info!(
                        "connection refused (HTTP {code}) while others work; limiting to {limit} connections"
                    );
                    self.limit_to(limit);
                }
                t.release(idx, true, now);
            }
            Err(e) if e.is_fatal() || matches!(e, Error::RangeIgnored) => {
                t.release(idx, false, now);
                self.fatal.get_or_insert(e);
            }
            Err(e) => self.failed(&mut t, idx, e, now),
        }
    }

    /// The server allows `limit` connections: no more are started, and the next downloads
    /// from the site start with that many.
    fn limit_to(&mut self, limit: usize) {
        self.conn_limit = limit;
        self.job.opts.learn.limited_to(limit);
        self.health.server_limit = Some(limit);
    }

    /// A connection failed in a way another attempt may fix: its segment is tried again,
    /// unless it has failed too often.
    fn failed(&mut self, t: &mut Table, idx: usize, e: Error, now: Instant) {
        tracing::debug!(segment = idx, "connection failed: {e}");
        if let Error::Busy(code) = e {
            let limit = t.active().saturating_sub(1).max(1);
            if limit < self.conn_limit {
                tracing::info!("server busy (HTTP {code}); limiting to {limit} connections");
                self.limit_to(limit);
            }
        } else {
            self.health.reconnects += 1;
        }
        t.release(idx, true, now);
        if self.single {
            t.rewind(idx);
        }
        if t.segs[idx].failures > self.job.opts.max_failures {
            self.fatal.get_or_insert(Error::TooManyFailures {
                attempts: t.segs[idx].failures,
                last: e.to_string(),
            });
        }
    }

    /// Every tick: the connections steered and new ones started, the progress reported, and
    /// now and then a checkpoint.
    pub(crate) async fn tick(&mut self, progress: Option<&watch::Sender<Progress>>) -> Result<()> {
        let now = Instant::now();
        let to_start = self.steer(now, progress);
        self.last_tick = now;
        for idx in to_start {
            let slot = self.free_slots.pop().unwrap_or_else(|| {
                self.next_slot += 1;
                self.next_slot - 1
            });
            self.spawn(idx, slot, None);
        }
        if self.fatal.is_none() && now - self.last_checkpoint >= self.job.opts.checkpoint_every {
            self.job.checkpoint(&self.ctx, false).await?;
            self.last_checkpoint = now;
        }
        Ok(())
    }

    /// Measures the speeds, steers the sources and connections, reports the progress; the
    /// segments to start connections for.
    fn steer(&mut self, now: Instant, progress: Option<&watch::Sender<Progress>>) -> Vec<usize> {
        let ctx = Arc::clone(&self.ctx);
        let mut t = ctx.table();
        let saturated_before = ctx.limiters.iter().any(Limiter::is_saturated);
        t.sample_speeds(now - self.last_tick);
        ctx.measure_sources(&t, &self.running);
        if !saturated_before {
            self.steer_sources(&t, now);
        }
        // A saturated limit sets the speed by itself: stealing would only waste
        // what the victims' abandoned responses buffered. Crawlers are still
        // replaced: that fixes the server, at the cost of one response.
        let saturated = ctx.limiters.iter().any(Limiter::is_saturated);
        let to_start = self.add_connections(&mut t, now, saturated);
        self.drop_laggard(&mut t, now, saturated);
        let active = t.active();
        self.job
            .opts
            .learn
            .note(|l| l.peak_connections = l.peak_connections.max(active));
        self.health.disk_wait = self.disk.tick(&ctx, active);
        if let Some(p) = progress {
            p.send_replace(Progress {
                health: self.health.clone(),
                ..snapshot(&t, ctx.size)
            });
        }
        to_start
    }

    /// Drops the sources and moves the connections that are much slower than the best.
    fn steer_sources(&mut self, t: &Table, now: Instant) {
        // A mirror whose every connection has crawled for a while next to the
        // best this download has seen is dropped as a whole: once the fast
        // sources are done, its connections would only be compared with each
        // other and keep the end of the file waiting.
        for source in slow_mirrors(t, &self.running, now) {
            if self.ctx.source_slow(source) {
                stop_source(&self.running, source);
            }
        }
        // Connections on a source that gives a connection far less than the
        // best one move: their ranges go back to the table and the new
        // connections go to the best source. The speeds are measured again
        // before the next moves, so a best source that slows down under
        // the new load stops them.
        if self.fatal.is_none() && now - self.last_move >= MOVE_EVERY {
            let moving = self.ctx.connections_to_move(t, &self.running, now);
            for idx in &moving {
                if let Some((stop, _, _)) = self.running.get(idx) {
                    tracing::debug!(segment = idx, "moving a connection to a faster source");
                    stop.cancel();
                }
            }
            if !moving.is_empty() {
                self.last_move = now;
            }
        }
    }

    /// Assigns segments to as many new connections as are allowed and needed.
    fn add_connections(&mut self, t: &mut Table, now: Instant, saturated: bool) -> Vec<usize> {
        // Under an unsaturated limit, only as many connections are added as the
        // missing speed needs at the typical per-connection speed: every extra
        // one throttled later fills socket buffers that a steal throws away.
        let limit = self.ctx.tightest_limit();
        let allowed = limit
            .map_or(self.conn_limit, |r| {
                self.conn_limit.min(
                    usize::try_from(r / MIN_LIMITED_CONN_RATE)
                        .unwrap_or(usize::MAX)
                        .max(1),
                )
            })
            .min(self.ctx.cap_while_checking(self.conn_limit));
        // Lowering the limit below that while more run (a limit set during
        // the download): the surplus closes between two requests, which costs
        // nothing.
        self.ctx
            .shed
            .store(t.active().saturating_sub(allowed), Ordering::Release);
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
        let mut to_start = Vec::new();
        if self.fatal.is_none() {
            while t.active() < allowed && budget > 0 {
                budget -= 1;
                match t.assign(self.next_id, now, &self.job.opts.split, !saturated) {
                    Some(idx) => {
                        self.next_id += 1;
                        to_start.push(idx);
                    }
                    None => break,
                }
            }
        }
        to_start
    }

    /// Drops the connection that crawls far behind the others, if one does.
    fn drop_laggard(&self, t: &mut Table, now: Instant, saturated: bool) {
        // Under a saturated limit a slow connection is slowed by the limit,
        // not the server: dropping it gains nothing and throws away what
        // it buffered.
        if !self.single
            && !saturated
            && self.fatal.is_none()
            && let Some(idx) = t.laggard(now)
            && let Some((stop, _, source)) =
                self.running.get(&idx).filter(|(s, _, _)| !s.is_cancelled())
        {
            t.segs[idx].evictions += 1;
            if self.ctx.source_slow(*source) {
                stop_source(&self.running, *source);
            }
            tracing::debug!(
                segment = idx,
                speed = t.segs[idx].speed,
                "dropping crawling connection"
            );
            stop.cancel();
        }
    }
}
