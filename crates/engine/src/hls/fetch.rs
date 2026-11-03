//! Downloading a stream: its playlists, segments in parallel (decrypted, in order), only
//! those of a clip, and joining the tracks (with ffmpeg, or as one `.ts`).

use super::*;

/// Fetches a playlist and follows a master playlist to its best variant.
pub(crate) async fn media_playlist(
    http: &Http,
    url: &str,
) -> Result<(MediaPlaylist, Option<String>)> {
    let (text, final_url) = http.text(url, MAX_PLAYLIST).await?;
    match parse_playlist(&final_url, &text)? {
        Playlist::Media(m) => Ok((m, None)),
        Playlist::Best { video, audio } => {
            let (text, final_url) = http.text(&video, MAX_PLAYLIST).await?;
            match parse_playlist(&final_url, &text)? {
                Playlist::Media(m) => Ok((m, audio)),
                Playlist::Best { .. } => {
                    Err(Error::State("a master playlist names another".into()))
                }
            }
        }
    }
}

/// Shared by the segment workers.
pub(crate) struct Run {
    pub(crate) http: Http,
    pub(crate) dir: PathBuf,
    pub(crate) keys: Mutex<HashMap<String, [u8; 16]>>,
    pub(crate) received: AtomicU64,
    pub(crate) done: AtomicUsize,
    pub(crate) active: AtomicUsize,
    pub(crate) opts: Options,
    pub(crate) cancel: CancellationToken,
}

impl Run {
    pub(crate) async fn key(&self, url: &str) -> Result<[u8; 16]> {
        let mut keys = self.keys.lock().await;
        if let Some(k) = keys.get(url) {
            return Ok(*k);
        }
        let resp = self.http.segment(url, None, 0).await?;
        let bytes = resp.bytes().await?;
        let key: [u8; 16] = bytes
            .as_ref()
            .try_into()
            .map_err(|_| Error::State(format!("the key at {url} is not 16 bytes")))?;
        keys.insert(url.to_owned(), key);
        Ok(key)
    }

    /// Downloads one segment into `path`, retrying failures that may pass.
    pub(crate) async fn fetch(&self, seg: &Segment, path: &Path, slot: u64) -> Result<()> {
        let mut failures = 0u32;
        loop {
            match self.try_fetch(seg, path, slot).await {
                Ok(()) => return Ok(()),
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(e) if e.is_fatal() || matches!(e, Error::Status(400..=499)) => {
                    return Err(e);
                }
                Err(e) => {
                    failures += 1;
                    if failures >= self.opts.max_failures {
                        return Err(Error::TooManyFailures {
                            attempts: failures,
                            last: e.to_string(),
                        });
                    }
                    let wait = Duration::from_millis(250 << failures.min(5));
                    tokio::select! {
                        () = tokio::time::sleep(wait) => {}
                        () = self.cancel.cancelled() => return Err(Error::Cancelled),
                    }
                }
            }
        }
    }

    pub(crate) async fn try_fetch(&self, seg: &Segment, path: &Path, slot: u64) -> Result<()> {
        let resp = tokio::select! {
            r = self.http.segment(&seg.url, seg.range, slot) => r?,
            () = self.cancel.cancelled() => return Err(Error::Cancelled),
        };
        let mut stream = resp.bytes_stream();
        let mut data = Vec::new();
        let mut got = 0u64;
        loop {
            let next = tokio::select! {
                n = tokio::time::timeout(self.opts.read_timeout, stream.next()) => n,
                () = self.cancel.cancelled() => {
                    self.received.fetch_sub(got, Ordering::Relaxed);
                    return Err(Error::Cancelled);
                }
            };
            match next {
                Err(_) => {
                    self.received.fetch_sub(got, Ordering::Relaxed);
                    return Err(Error::Stalled(self.opts.read_timeout));
                }
                Ok(None) => break,
                Ok(Some(chunk)) => {
                    let chunk = match chunk {
                        Ok(c) => c,
                        Err(e) => {
                            self.received.fetch_sub(got, Ordering::Relaxed);
                            return Err(e.into());
                        }
                    };
                    for l in &self.opts.limiters {
                        l.acquire(chunk.len()).await;
                    }
                    got += chunk.len() as u64;
                    self.received
                        .fetch_add(chunk.len() as u64, Ordering::Relaxed);
                    data.extend_from_slice(&chunk);
                }
            }
        }
        if let Some((start, end)) = seg.range
            && data.len() as u64 != end - start
        {
            self.received.fetch_sub(got, Ordering::Relaxed);
            return Err(Error::Truncated);
        }
        if let Some(k) = &seg.key {
            use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
            let key = self.key(&k.url).await?;
            data = cbc::Decryptor::<aes::Aes128>::new(&key.into(), &k.iv.into())
                .decrypt_padded_vec::<Pkcs7>(&data)
                .map_err(|_| Error::State(format!("{} did not decrypt", seg.url)))?;
        }
        let part = sibling(path, ".part");
        tokio::fs::write(&part, &data).await?;
        tokio::fs::rename(&part, path).await?;
        Ok(())
    }
}

/// One track's files in order: the init segment (if any), then the media segments.
/// The segment files of a track, the init segment first; with `clip`, only the segments the
/// clip falls in (named by their place in the whole track, so a resumed clip finds them).
pub(crate) fn track_files(
    dir: &Path,
    track: &str,
    playlist: &MediaPlaylist,
    clip: Option<Clip>,
) -> Vec<(Segment, PathBuf)> {
    let kept = clip.map(|c| window(playlist, c).0);
    playlist
        .init
        .iter()
        .map(|s| (s.clone(), dir.join(format!("{track}-init.seg"))))
        .chain(
            playlist
                .segments
                .iter()
                .enumerate()
                .filter(|(i, _)| kept.as_ref().is_none_or(|k| k.contains(i)))
                .map(|(i, s)| (s.clone(), dir.join(format!("{track}-{i:06}.seg")))),
        )
        .collect()
}

/// The segments `clip` falls in, and when the first of them starts. Without every segment's
/// length the whole track is kept, starting at 0.
pub(crate) fn window(playlist: &MediaPlaylist, clip: Clip) -> (Vec<usize>, f64) {
    let all = || ((0..playlist.segments.len()).collect(), 0.0);
    let Some(lengths) = playlist
        .segments
        .iter()
        .map(|s| s.duration)
        .collect::<Option<Vec<f64>>>()
    else {
        return all();
    };
    let mut kept = Vec::new();
    let mut first = None;
    let mut t = 0.0;
    for (i, d) in lengths.into_iter().enumerate() {
        if t < clip.end && t + d > clip.start {
            kept.push(i);
            first.get_or_insert(t);
        }
        t += d;
    }
    match first {
        Some(at) => (kept, at),
        // The clip is past the end: the last segment, so the cut says so.
        None => all(),
    }
}

/// Downloads an HLS stream to `req.output`. Interrupted, it keeps the finished segments,
/// and calling again continues.
pub async fn download_hls(
    req: &StreamRequest,
    opts: &Options,
    progress: Option<&watch::Sender<Progress>>,
    cancel: &CancellationToken,
) -> Result<Outcome> {
    let started = Instant::now();
    let _lock = crate::place::Lock::acquire(&req.output)?;
    let http = Http::new(&http_config(&req.headers, opts))?;
    if let Some(p) = progress {
        p.send_modify(|p| p.phase = Phase::Probing);
    }
    let (video, master_audio) = media_playlist(&http, &req.video).await?;
    let audio_url = req.audio.clone().or(master_audio);
    let audio = match &audio_url {
        Some(u) => Some(media_playlist(&http, u).await?.0),
        None => None,
    };
    if audio.is_some() && req.ffmpeg.is_none() {
        return Err(Error::State(
            "this stream has separate audio: ffmpeg is needed to join it".into(),
        ));
    }
    if req.clip.is_some() && req.ffmpeg.is_none() {
        return Err(Error::State("cutting a clip needs ffmpeg".into()));
    }

    let mut tracks = vec![("video", Source::Segments(video))];
    if let Some(a) = audio {
        tracks.push(("audio", Source::Segments(a)));
    }
    let outcome = segmented(
        http,
        &req.output,
        tracks,
        req.ffmpeg.as_deref(),
        req.clip,
        opts,
        progress,
        cancel,
        started,
    )
    .await?;
    if opts.mark_of_the_web {
        crate::mark_of_the_web(&req.output, &req.video, &req.headers);
    }
    Ok(outcome)
}

/// A track to put into the output: segments still to download (and join), or a file that
/// is already whole (a DASH representation that is a single file).
pub(crate) enum Source {
    Segments(MediaPlaylist),
    Joined(PathBuf),
}

/// Downloads the segments of `tracks` into `<output>.shardload-hls/`, joins each track, and puts
/// the first (video) and the second (audio, if any) into `output`, with ffmpeg when given.
/// With `clip` (and ffmpeg), only the segments it falls in, and the result cut to it.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn segmented(
    http: Http,
    output: &Path,
    tracks: Vec<(&'static str, Source)>,
    ffmpeg: Option<&Path>,
    clip: Option<Clip>,
    opts: &Options,
    progress: Option<&watch::Sender<Progress>>,
    cancel: &CancellationToken,
    started: Instant,
) -> Result<Outcome> {
    let dir = sibling(output, ".shardload-hls");
    tokio::fs::create_dir_all(&dir).await?;
    let mut files = Vec::new();
    for (name, source) in &tracks {
        if let Source::Segments(p) = source {
            files.extend(track_files(&dir, name, p, clip));
        }
    }
    let total = files.len();
    let missing: Vec<(Segment, PathBuf)> =
        files.iter().filter(|(_, p)| !p.exists()).cloned().collect();
    let resumed_from: u64 = files
        .iter()
        .filter(|(_, p)| p.exists())
        .filter_map(|(_, p)| std::fs::metadata(p).ok().map(|m| m.len()))
        .sum();

    let run = Arc::new(Run {
        http,
        dir: dir.clone(),
        keys: Mutex::new(HashMap::new()),
        received: AtomicU64::new(resumed_from),
        done: AtomicUsize::new(total - missing.len()),
        active: AtomicUsize::new(0),
        opts: opts.clone(),
        cancel: cancel.child_token(),
    });

    let reporter = progress.map(|tx| report(Arc::clone(&run), tx.clone(), total));

    let parallel = opts.connections.clamp(1, MAX_PARALLEL);
    let results: Vec<Result<()>> = futures_util::stream::iter(missing.into_iter().enumerate())
        .map(|(n, (seg, path))| {
            let run = Arc::clone(&run);
            async move {
                run.active.fetch_add(1, Ordering::Relaxed);
                let r = run.fetch(&seg, &path, (n % parallel) as u64).await;
                run.active.fetch_sub(1, Ordering::Relaxed);
                if r.is_ok() {
                    run.done.fetch_add(1, Ordering::Relaxed);
                } else {
                    // One segment failing for good stops the rest.
                    run.cancel.cancel();
                }
                r
            }
        })
        .buffer_unordered(parallel)
        .collect()
        .await;
    run.cancel.cancel();
    if let Some(r) = reporter {
        r.await.ok();
    }
    if let Some(e) = results
        .into_iter()
        .filter_map(Result::err)
        .find(|e| !matches!(e, Error::Cancelled))
    {
        return Err(e);
    }
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }

    // Join each track, then put them together. Each track's joined file starts where its
    // first segment does, which with a clip is somewhat before the clip.
    let mut joined = Vec::new();
    for (name, source) in &tracks {
        joined.push(match source {
            Source::Segments(p) => {
                let path = dir.join(format!("{name}.joined"));
                join(&track_files(&dir, name, p, clip), &path).await?;
                let starts_at = clip.map_or(0.0, |c| window(p, c).1);
                (path, starts_at)
            }
            Source::Joined(path) => (path.clone(), 0.0),
        });
    }
    match ffmpeg {
        Some(ffmpeg) => {
            if let Some(p) = progress {
                p.send_modify(|p| p.phase = Phase::Verifying);
            }
            mux(ffmpeg, &joined[0], joined.get(1), output, clip).await?;
        }
        None => tokio::fs::rename(&joined[0].0, output).await?,
    }
    tokio::fs::remove_dir_all(&run.dir).await.ok();
    let size = tokio::fs::metadata(output).await?.len();
    if let Some(p) = progress {
        p.send_modify(|p| {
            p.phase = Phase::Done;
            p.size = Some(size);
            p.received = size;
        });
    }
    Ok(Outcome {
        size,
        resumed_from,
        elapsed: started.elapsed(),
        checksum: None,
        checksum_from: None,
        structure: None,
    })
}

/// Reports the progress of `run`, which has `total` segments, until it is cancelled.
fn report(run: Arc<Run>, tx: watch::Sender<Progress>, total: usize) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last = (Instant::now(), run.received.load(Ordering::Relaxed));
        let mut speed = 0.0;
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let received = run.received.load(Ordering::Relaxed);
            let done = run.done.load(Ordering::Relaxed);
            let dt = last.0.elapsed().as_secs_f64().max(1e-3);
            let now = received.saturating_sub(last.1) as f64 / dt;
            speed = if speed == 0.0 {
                now
            } else {
                speed * 0.7 + now * 0.3
            };
            last = (Instant::now(), received);
            // Segments are about the same size: estimate the total from those done.
            let size = (done > 0).then(|| received / done as u64 * total as u64);
            tx.send_modify(|p| {
                p.phase = Phase::Downloading;
                p.size = size.map(|s| s.max(received));
                p.received = received;
                p.speed = speed;
                p.connections = run.active.load(Ordering::Relaxed);
                p.segments.clear();
            });
            if run.cancel.is_cancelled() {
                return;
            }
        }
    })
}

pub(crate) async fn join(files: &[(Segment, PathBuf)], out: &Path) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut f = tokio::fs::File::create(out).await?;
    for (_, path) in files {
        let mut src = tokio::fs::File::open(path).await?;
        tokio::io::copy(&mut src, &mut f).await?;
    }
    f.flush().await?;
    Ok(())
}

/// Puts the joined tracks into `out`, each given with the time it starts at in the stream:
/// copied as they are, or with `clip` only that part of them (re-encoded when exact).
pub(crate) async fn mux(
    ffmpeg: &Path,
    video: &(PathBuf, f64),
    audio: Option<&(PathBuf, f64)>,
    out: &Path,
    clip: Option<Clip>,
) -> Result<()> {
    let mut cmd = tokio::process::Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    let input = |cmd: &mut tokio::process::Command, (path, starts_at): &(PathBuf, f64)| {
        if let Some(c) = clip {
            cmd.arg("-ss").arg(seconds((c.start - starts_at).max(0.0)));
        }
        cmd.arg("-i").arg(path);
    };
    input(&mut cmd, video);
    if let Some(a) = audio {
        input(&mut cmd, a);
        cmd.args(["-map", "0:v:0?", "-map", "1:a:0?"]);
    } else {
        cmd.args(["-map", "0"]);
    }
    if let Some(c) = clip {
        cmd.arg("-t").arg(seconds((c.end - c.start).max(0.0)));
    }
    match clip {
        Some(c) if c.exact => {
            cmd.args([
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-c:a", "aac", "-b:a",
                "192k",
            ]);
        }
        // ADTS AAC from MPEG-TS needs its headers rewritten for MP4.
        _ => {
            cmd.args(["-c", "copy", "-bsf:a", "aac_adtstoasc"]);
        }
    }
    cmd.arg(out);
    cmd.stdin(std::process::Stdio::null());
    let output = cmd.output().await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::State(format!(
            "ffmpeg could not join the stream: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// `12.345` for ffmpeg.
pub(crate) fn seconds(s: f64) -> String {
    format!("{s:.3}")
}
