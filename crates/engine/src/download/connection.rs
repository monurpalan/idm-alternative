//! One connection: its requests, what it receives, writing it in large chunks, paying the
//! speed limits, and checking mirrors against each other.

use super::*;

/// One connection: downloads its segment until it reaches the segment's end (which may
/// move closer while it runs, when another connection steals the rest), buffering data
/// into large positional writes.
pub(crate) async fn connection(
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
pub(crate) async fn check_mirrors(ctx: &Ctx, primary: &Remote, urls: &[String]) {
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
pub(crate) fn slow_mirrors(
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
pub(crate) fn stop_source(
    running: &HashMap<usize, (CancellationToken, u64, usize)>,
    source: usize,
) {
    for (stop, _, s) in running.values() {
        if *s == source {
            stop.cancel();
        }
    }
}

/// Asks `source` for `start..end` of the file (`end` = `u64::MAX`: to the end). The
/// download's own URL is resolved again once if it has expired; a mirror is not.
pub(crate) async fn request(
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
pub(crate) async fn receive(
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
pub(crate) async fn pay(ctx: &Ctx, bytes: usize, stop: &CancellationToken) {
    for limiter in &ctx.limiters {
        tokio::select! {
            () = stop.cancelled() => return,
            () = limiter.acquire(bytes) => {}
        }
    }
}

pub(crate) struct Writer {
    pub(crate) buf: Vec<u8>,
    pub(crate) offset: u64,
    pub(crate) last: Instant,
}

impl Writer {
    pub(crate) async fn flush(&mut self, ctx: &Ctx, idx: usize) -> Result<()> {
        self.last = Instant::now();
        if self.buf.is_empty() {
            return Ok(());
        }
        let len = self.buf.len() as u64;
        let data = std::mem::take(&mut self.buf);
        let mut data = ctx.write_counted(data, self.offset).await?;
        self.offset += len;
        ctx.table().segs[idx].written = self.offset;
        data.clear();
        self.buf = data;
        Ok(())
    }
}
