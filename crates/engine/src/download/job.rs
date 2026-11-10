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
        let primary = remote.clone();
        let ctx = self.context(remote, table, mirrors.len(), cancel)?;

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
        let mut steer = Steer {
            job: self,
            ctx: Arc::clone(&ctx),
            single,
            workers: JoinSet::new(),
            running: HashMap::new(),
            free_slots: Vec::new(),
            next_slot: 1,
            next_id: 1,
            conn_limit: if single {
                1
            } else {
                self.opts.connections.max(1)
            },
            fatal: None,
            last_tick: Instant::now(),
            last_checkpoint: Instant::now(),
            last_move: Instant::now(),
            health: Health::default(),
            disk: DiskGauge::default(),
        };
        let (idx, resp) = first_resp;
        ctx.table().claim(idx, steer.next_id, Instant::now());
        steer.next_id += 1;
        steer.spawn(idx, 0, Some(resp));

        steer.drive(progress, cancel).await?;
        let Steer { fatal, .. } = steer;

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
        // A checkpoint still being written must not leave a state file beside the finished
        // one; it is not waited for.
        *ctx.finished.lock().unwrap_or_else(PoisonError::into_inner) = true;
        // Every connection task has finished, so this is the last handle to the file and it
        // is closed here, before the caller renames it.
        drop(ctx);
        Ok((received, resumed_from))
    }

    /// What the download's connections share: the file, the table of segments, the sources
    /// (its own URL and `mirrors` mirrors).
    fn context(
        &self,
        remote: Remote,
        table: Table,
        mirrors: usize,
        cancel: &CancellationToken,
    ) -> Result<Arc<Ctx>> {
        let sources = Mutex::new((0..=mirrors).map(|_| Source::default()).collect());
        Ok(Arc::new(Ctx {
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
            saving: Mutex::new(None),
            finished: Arc::new(Mutex::new(false)),
            disk_writes: AtomicUsize::new(0),
            slow_disk: self.opts.slow_disk,
            max_request: self.opts.max_request,
        }))
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
        let Some((remote, resp)) = self
            .probe_resume(start, state.validators.if_range())
            .await?
        else {
            return Ok(None);
        };
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
        let Some((remote, resp)) = self.probe_resume(start, None).await? else {
            return Ok(None);
        };
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

    /// The first request when resuming at `start`; `None` when the server's file now ends
    /// before that (416), so it is not the one partly downloaded.
    async fn probe_resume(
        &self,
        start: u64,
        if_range: Option<&str>,
    ) -> Result<Option<(Remote, Response)>> {
        match self.http.probe(&self.req.url, start, if_range, 0).await {
            Err(Error::Status(416)) => {
                tracing::info!("remote file is shorter than what was downloaded; starting over");
                Ok(None)
            }
            r => r.map(Some),
        }
    }

    /// Saves progress. Positions are captured before the state is written, so the state
    /// never claims bytes that were not yet handed to the OS: that survives the process
    /// crashing or being killed. With `durable`, the file is also flushed to the disk first,
    /// which additionally survives power loss — but on Windows a flush stalls every write
    /// to the file for seconds, so it is only done when stopping (see ADR 0002). The state
    /// file is written on a blocking thread while the download goes on; a save that fails
    /// is reported at the next checkpoint.
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
        let path = self.state_path.to_owned();
        let finished = Arc::clone(&ctx.finished);
        let save = move || state.save_unless(&path, &finished);
        if durable {
            // After the one being written, so the clean state is the one that stays.
            ctx.saved().await.ok();
            return Ok(tokio::task::spawn_blocking(save)
                .await
                .map_err(std::io::Error::other)??);
        }
        match ctx.take_saving() {
            // The last one is still being written: this one is skipped, the next is newer.
            Some(h) if !h.is_finished() => {
                *ctx.saving.lock().unwrap_or_else(PoisonError::into_inner) = Some(h);
                return Ok(());
            }
            Some(h) => h.await.map_err(std::io::Error::other)??,
            None => {}
        }
        *ctx.saving.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(tokio::task::spawn_blocking(save));
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
