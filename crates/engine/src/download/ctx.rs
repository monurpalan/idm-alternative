//! What the controller and every connection share: the segment table, the sources (the
//! link and its mirrors) and how connections are spread over them.

use super::*;

/// Shared by the controller and every connection.
pub(crate) struct Ctx {
    pub(crate) http: Http,
    /// Where requests go now; replaced when an expired redirect target is resolved again.
    pub(crate) remote: RwLock<Remote>,
    /// Bumped on every replacement of `remote`.
    pub(crate) generation: AtomicU64,
    /// Held while resolving the original URL again, so connections that fail together
    /// trigger it once.
    pub(crate) relinking: tokio::sync::Mutex<()>,
    /// The URL the user asked for, before redirects.
    pub(crate) original_url: String,
    pub(crate) size: Option<u64>,
    pub(crate) ranges: bool,
    pub(crate) file: OutputFile,
    pub(crate) table: Mutex<Table>,
    pub(crate) read_timeout: Duration,
    pub(crate) limiters: Vec<Limiter>,
    /// How many connections should close at the end of their current request, because a
    /// speed limit needs fewer than are open.
    pub(crate) shed: AtomicUsize,
    /// Source 0 is `remote`; then one per mirror, in use once it is shown to serve the
    /// same bytes.
    pub(crate) sources: Mutex<Vec<Source>>,
    pub(crate) cancel: CancellationToken,
    /// A checkpoint being written on a blocking thread: the state file is synced to the
    /// disk, which on a slow disk takes long enough to hold the controller up.
    pub(crate) saving: Mutex<Option<tokio::task::JoinHandle<std::io::Result<()>>>>,
    /// Set when the download has all its data: a checkpoint still being written then leaves
    /// the state file alone (see [`State::save_unless`]).
    pub(crate) finished: Arc<Mutex<bool>>,
    /// Writes to the file in progress (see [`DiskGauge`]).
    pub(crate) disk_writes: AtomicUsize,
    /// See [`Options::slow_disk`].
    pub(crate) slow_disk: Option<Duration>,
}

/// How one source of the file is doing: the download's own URL (0) or a mirror.
#[derive(Default)]
pub(crate) struct Source {
    /// A mirror's answer once it is checked; `None` while it is being checked (and for
    /// source 0, whose address is `Ctx::remote`).
    pub(crate) remote: Option<Remote>,
    pub(crate) active: usize,
    /// Failed requests in a row.
    pub(crate) failures: u32,
    /// Connections dropped for crawling.
    pub(crate) slow: u32,
    pub(crate) dropped: bool,
    /// A mirror got its first connection, which measures it.
    pub(crate) tried: bool,
    /// Bytes per second per connection, as last measured.
    pub(crate) speed: f64,
}

impl Ctx {
    pub(crate) fn table(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The checkpoint being written, if any.
    pub(crate) fn take_saving(&self) -> Option<tokio::task::JoinHandle<std::io::Result<()>>> {
        self.saving
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Waits for the checkpoint being written, if any, and says how it went.
    pub(crate) async fn saved(&self) -> Result<()> {
        match self.take_saving() {
            Some(h) => Ok(h.await.map_err(std::io::Error::other)??),
            None => Ok(()),
        }
    }

    /// The lowest active speed limit, if any.
    pub(crate) fn tightest_limit(&self) -> Option<u64> {
        self.limiters.iter().filter_map(Limiter::rate).min()
    }

    /// Where the next request for `pos..end` should stop. Under a speed limit, requests ask
    /// for half a second of the limit at most: a throttled connection fills the buffers
    /// between it and the server, and whatever is buffered past the point where its range
    /// shrinks or it closes is thrown away. Small requests bound that loss and let a
    /// connection close between two of them without any.
    pub(crate) fn window_end(&self, pos: u64, end: u64) -> u64 {
        match self.tightest_limit() {
            Some(rate) if self.ranges => end.min(pos + (rate / 2).clamp(MIN_WINDOW, MAX_WINDOW)),
            _ => end,
        }
    }

    /// Whether this connection should close now, claiming one of the requested closes.
    pub(crate) fn take_shed(&self) -> bool {
        self.shed
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .is_ok()
    }

    pub(crate) fn sources(&self) -> MutexGuard<'_, Vec<Source>> {
        self.sources.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The source for a new connection. A mirror just shown to serve the file gets one
    /// connection to measure it; after that each connection goes to the source that gives a
    /// connection the most, so mirrors only take connections where they add speed (a server
    /// that limits each client gives each connection less as more of them ask).
    pub(crate) fn pick_source(&self) -> usize {
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
    pub(crate) fn connections_to_move(
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
    pub(crate) fn measure_sources(
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
    pub(crate) fn cap_while_checking(&self, limit: usize) -> usize {
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

    pub(crate) fn source_done(&self, source: usize, ok: bool) {
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
    pub(crate) fn source_failed(&self, source: usize, e: &Error) -> bool {
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
    pub(crate) fn source_slow(&self, source: usize) -> bool {
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
    pub(crate) fn remote(&self) -> (Remote, u64) {
        let remote = self.remote.read().unwrap_or_else(PoisonError::into_inner);
        (remote.clone(), self.generation.load(Ordering::Acquire))
    }

    /// A request to the current URL was refused with `code` (401/403/410). If that URL is a
    /// redirect target, resolves the original URL again — redirect targets are often signed
    /// links that expire while the original stays valid — and switches to the new target
    /// once it is shown to serve the same file. `seen` is the generation the failed request
    /// used: when another connection already switched, there is nothing to do.
    pub(crate) async fn relink(&self, seen: u64, code: u16, slot: u64) -> Result<()> {
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
