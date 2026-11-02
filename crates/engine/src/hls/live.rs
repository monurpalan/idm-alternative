//! Recording a live stream: reading its playlist again and again, appending new segments
//! until it ends, the user stops it or the time limit.

use super::*;

/// What to record: a live HLS stream (a master playlist, whose best variant is taken, or a
/// media playlist), an optional separate audio playlist, and where the recording goes.
#[derive(Debug, Clone)]
pub struct RecordRequest {
    pub video: String,
    pub audio: Option<String>,
    pub output: PathBuf,
    pub headers: Vec<(String, String)>,
    /// Puts the recording into an MP4 (and joins separate audio); without it the video
    /// track's segments are joined as they are (`.ts`).
    pub ffmpeg: Option<PathBuf>,
    /// Stop once this much has been recorded.
    pub limit: Option<Duration>,
}

/// Consecutive failed reads of a live playlist after which the recording stops.
pub(crate) const LIVE_READ_FAILURES: u32 = 5;
/// How often a live playlist is read when it does not say (no target duration).
pub(crate) const LIVE_REFRESH: Duration = Duration::from_secs(2);

/// Where a live track's segments are listed.
pub(crate) enum LiveSource {
    /// An HLS media playlist.
    Hls(String),
    /// A representation of a DASH manifest.
    Dash { manifest: String, rep: String },
}

/// What a live track offers when its list is read: the segments numbered from `first`.
pub(crate) struct LiveView {
    pub(crate) init: Option<Segment>,
    pub(crate) first: u64,
    pub(crate) segments: Vec<Segment>,
    pub(crate) ended: bool,
    /// Read again after this long, seconds, when the list says.
    pub(crate) refresh: Option<f64>,
}

impl LiveSource {
    pub(crate) async fn read(&self, http: &Http) -> Result<LiveView> {
        match self {
            Self::Hls(url) => {
                let (text, url) = http.text(url, MAX_PLAYLIST).await?;
                match parse_any(&url, &text)? {
                    Playlist::Media(m) => Ok(LiveView {
                        init: m.init,
                        first: m.sequence,
                        segments: m.segments,
                        ended: m.ended,
                        refresh: m.target,
                    }),
                    Playlist::Best { .. } => {
                        Err(Error::State("a master playlist names another".into()))
                    }
                }
            }
            Self::Dash { manifest, rep } => {
                let (text, url) = http.text(manifest, MAX_PLAYLIST).await?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                let live = shardload_media::dash_live(&url, &text, rep, now)
                    .map_err(|e| Error::State(e.0))?;
                let segment = |p: shardload_media::Piece| Segment {
                    url: p.url,
                    range: p.range,
                    key: None,
                    duration: p.duration,
                };
                Ok(LiveView {
                    init: live.track.init.map(segment),
                    first: live.track.first,
                    segments: live.track.segments.into_iter().map(segment).collect(),
                    ended: live.ended,
                    refresh: live.refresh,
                })
            }
        }
    }
}

/// One track of a recording: its list, the file its segments are appended to, and the
/// next segment wanted.
pub(crate) struct LiveTrack {
    pub(crate) name: &'static str,
    pub(crate) source: LiveSource,
    pub(crate) file: PathBuf,
    pub(crate) next: Option<u64>,
    /// Seconds recorded.
    pub(crate) recorded: f64,
    pub(crate) ended: bool,
}

/// Records a live HLS stream to `req.output`: the playlist is read again every target
/// duration and each new segment (by media sequence number) is appended. Segments that left
/// the playlist's window before they were read are lost; how many is logged and reported
/// as [`Progress::missed`]. It ends when the stream does (`#EXT-X-ENDLIST`), when
/// `req.limit` is recorded, when `cancel` fires (the user stopped it: what was recorded is
/// kept), or when the playlist cannot be read several times in a row. Then ffmpeg puts it
/// into the output.
pub async fn record_hls(
    req: &RecordRequest,
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
    let (text, final_url) = http.text(&req.video, MAX_PLAYLIST).await?;
    let (video_url, master_audio) = match parse_any(&final_url, &text)? {
        Playlist::Best { video, audio } => (video, audio),
        Playlist::Media(_) => (final_url, None),
    };
    let audio_url = req.audio.clone().or(master_audio);
    if audio_url.is_some() && req.ffmpeg.is_none() {
        return Err(Error::State(
            "this stream has separate audio: ffmpeg is needed to join it".into(),
        ));
    }
    let mut sources = vec![LiveSource::Hls(video_url)];
    sources.extend(audio_url.map(LiveSource::Hls));
    record(req, sources, http, opts, progress, cancel, started).await
}

/// Records the live tracks (video, then audio) of `req` (see [`record_hls`]).
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "one long sequence kept together; to be split when it next changes"
)]
pub(crate) async fn record(
    req: &RecordRequest,
    sources: Vec<LiveSource>,
    http: Http,
    opts: &Options,
    progress: Option<&watch::Sender<Progress>>,
    cancel: &CancellationToken,
    started: Instant,
) -> Result<Outcome> {
    let dir = sibling(&req.output, ".shardload-hls");
    // A recording cannot go on where it stopped: the stream has moved on.
    tokio::fs::remove_dir_all(&dir).await.ok();
    tokio::fs::create_dir_all(&dir).await?;
    let mut tracks: Vec<LiveTrack> = sources
        .into_iter()
        .zip(["video", "audio"])
        .map(|(source, name)| LiveTrack {
            name,
            source,
            file: dir.join(format!("{name}.joined")),
            next: None,
            recorded: 0.0,
            ended: false,
        })
        .collect();
    let run = Run {
        http,
        dir: dir.clone(),
        keys: Mutex::new(HashMap::new()),
        received: AtomicU64::new(0),
        done: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        opts: opts.clone(),
        cancel: cancel.child_token(),
    };

    let mut failures = 0u32;
    let mut missed = 0u64;
    let mut last = (Instant::now(), 0u64);
    let mut speed = 0.0;
    let mut stopped = false;
    'record: loop {
        let mut wait = LIVE_REFRESH;
        for t in tracks.iter_mut().filter(|t| !t.ended) {
            let media = match t.source.read(&run.http).await {
                Ok(m) => {
                    failures = 0;
                    m
                }
                Err(e) => {
                    failures += 1;
                    tracing::warn!(
                        track = t.name,
                        failures,
                        "could not read the live playlist: {e}"
                    );
                    if failures >= LIVE_READ_FAILURES {
                        if tracks.iter().all(|t| t.recorded == 0.0) {
                            return Err(e);
                        }
                        break 'record;
                    }
                    continue;
                }
            };
            if let Some(target) = media.refresh.filter(|r| *r > 0.0) {
                wait = wait.min(Duration::from_secs_f64(target));
            }
            // The initialisation segment goes first, once there are media segments after it
            // (a live DASH stream may have none out yet).
            if t.next.is_none()
                && !media.segments.is_empty()
                && let Some(init) = &media.init
                && append(&run, init, &t.file, t.name).await.is_err()
            {
                stopped = true;
                break 'record;
            }
            for (i, seg) in media.segments.iter().enumerate() {
                let seq = media.first + i as u64;
                if t.next.is_some_and(|n| seq < n) {
                    continue;
                }
                if let Some(n) = t.next.filter(|n| seq > *n) {
                    missed += seq - n;
                    tracing::warn!(
                        track = t.name,
                        lost = seq - n,
                        "segments left the live playlist before they were read"
                    );
                }
                match append(&run, seg, &t.file, t.name).await {
                    Ok(()) => {}
                    Err(Error::Cancelled) => {
                        stopped = true;
                        break 'record;
                    }
                    Err(e) => {
                        tracing::warn!(track = t.name, "a live segment failed: {e}");
                        missed += 1;
                    }
                }
                t.next = Some(seq + 1);
                t.recorded += seg.duration.unwrap_or(0.0);
            }
            t.ended = media.ended;
        }
        let recorded = tracks[0].recorded;
        if let Some(p) = progress {
            let received = run.received.load(Ordering::Relaxed);
            let dt = last.0.elapsed().as_secs_f64().max(1e-3);
            let now = received.saturating_sub(last.1) as f64 / dt;
            speed = if speed == 0.0 {
                now
            } else {
                speed * 0.7 + now * 0.3
            };
            last = (Instant::now(), received);
            p.send_modify(|p| {
                p.phase = Phase::Recording;
                p.size = None;
                p.received = received;
                p.speed = speed;
                p.connections = 1;
                p.recorded = recorded;
                p.missed = missed;
                p.segments.clear();
            });
        }
        if tracks.iter().all(|t| t.ended) || req.limit.is_some_and(|l| recorded >= l.as_secs_f64())
        {
            break;
        }
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = cancel.cancelled() => {
                stopped = true;
                break;
            }
        }
    }
    if stopped {
        tracing::info!("recording stopped");
    }
    if !tracks[0].file.exists() {
        tokio::fs::remove_dir_all(&dir).await.ok();
        return Err(Error::State("nothing was recorded".into()));
    }
    if let Some(p) = progress {
        p.send_modify(|p| p.phase = Phase::Verifying);
    }
    let joined: Vec<(PathBuf, f64)> = tracks
        .iter()
        .filter(|t| t.file.exists())
        .map(|t| (t.file.clone(), 0.0))
        .collect();
    match &req.ffmpeg {
        Some(ffmpeg) => {
            if let Err(e) = mux(ffmpeg, &joined[0], joined.get(1), &req.output, None).await {
                // A recording cannot be made again: keep the video as it came.
                let kept = req.output.with_extension("ts");
                tokio::fs::rename(&joined[0].0, &kept).await?;
                tokio::fs::remove_dir_all(&dir).await.ok();
                return Err(Error::State(format!(
                    "{e}; the recording is kept as it came, in {}",
                    kept.display()
                )));
            }
        }
        None => tokio::fs::rename(&joined[0].0, &req.output).await?,
    }
    tokio::fs::remove_dir_all(&dir).await.ok();
    let size = tokio::fs::metadata(&req.output).await?.len();
    if let Some(p) = progress {
        p.send_modify(|p| {
            p.phase = Phase::Done;
            p.size = Some(size);
            p.received = size;
        });
    }
    if opts.mark_of_the_web {
        crate::mark_of_the_web(&req.output, &req.video, &req.headers);
    }
    Ok(Outcome {
        size,
        resumed_from: 0,
        elapsed: started.elapsed(),
        checksum: None,
        checksum_from: None,
        structure: None,
    })
}

/// Downloads one segment of a live track and appends it to the track's file.
pub(crate) async fn append(run: &Run, seg: &Segment, file: &Path, track: &str) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let part = run.dir.join(format!("{track}-live.seg"));
    run.fetch(seg, &part, 0).await?;
    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .await?;
    let mut src = tokio::fs::File::open(&part).await?;
    tokio::io::copy(&mut src, &mut out).await?;
    out.flush().await?;
    drop(src);
    tokio::fs::remove_file(&part).await.ok();
    Ok(())
}
