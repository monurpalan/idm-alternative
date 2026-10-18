//! DASH: downloads the chosen video and audio representations of a manifest and puts them
//! into one file. Segmented representations go through the same parallel, resumable segment
//! downloader as HLS; a representation that is one file is downloaded like any file, with
//! several connections.

use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::download::{Options, Outcome, Phase, Progress, Request, http_config};
use crate::error::{Error, Result};
use crate::hls::{MediaPlaylist, Segment, Source, segmented};
use crate::remote::Http;
use crate::state::sibling;

#[derive(Debug, Clone)]
pub struct DashRequest {
    /// The manifest (MPD).
    pub manifest: String,
    /// Representation ids, as listed by `shardload_media::parse_mpd`.
    pub video: String,
    pub audio: Option<String>,
    pub output: PathBuf,
    pub headers: Vec<(String, String)>,
    /// Puts video and audio together; needed when `audio` is set.
    pub ffmpeg: Option<PathBuf>,
}

const MAX_MANIFEST: usize = 16 << 20;

fn segment(p: shardload_media::Piece) -> Segment {
    Segment {
        url: p.url,
        range: p.range,
        key: None,
    }
}

/// Downloads a DASH stream to `req.output`. Interrupted, it keeps what is finished, and
/// calling again continues.
pub async fn download_dash(
    req: &DashRequest,
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
    let (text, manifest_url) = http.text(&req.manifest, MAX_MANIFEST).await?;
    let track = |id: &str| {
        shardload_media::dash_track(&manifest_url, &text, id).map_err(|e| Error::State(e.0))
    };
    let mut wanted = vec![("video", track(&req.video)?)];
    if let Some(a) = &req.audio {
        if req.ffmpeg.is_none() {
            return Err(Error::State(
                "this stream has separate audio: ffmpeg is needed to join it".into(),
            ));
        }
        wanted.push(("audio", track(a)?));
    }

    let dir = sibling(&req.output, ".shardload-hls");
    tokio::fs::create_dir_all(&dir).await?;
    let mut tracks = Vec::new();
    for (name, t) in wanted {
        if t.single {
            // One file: the regular downloader, with its connections and resume. Already
            // whole from an earlier attempt when it exists without partial data.
            let path = dir.join(format!("{name}.joined"));
            let whole = path.exists() && !sibling(&path, ".shardload-part").exists();
            if !whole {
                let url = t.segments[0].url.clone();
                // A part of the result: the result is marked once it is put together. A
                // track of a stream: nobody publishes checksums for those.
                let part_opts = Options {
                    mark_of_the_web: false,
                    find_checksum: false,
                    ..opts.clone()
                };
                crate::download::download(
                    &Request {
                        url,
                        output: path.clone(),
                        headers: req.headers.clone(),
                        mirrors: Vec::new(),
                    },
                    &part_opts,
                    progress,
                    cancel,
                )
                .await?;
            }
            tracks.push((name, Source::Joined(path)));
        } else {
            tracks.push((
                name,
                Source::Segments(MediaPlaylist {
                    init: t.init.map(segment),
                    segments: t.segments.into_iter().map(segment).collect(),
                }),
            ));
        }
    }
    let outcome = segmented(
        http,
        &req.output,
        tracks,
        req.ffmpeg.as_deref(),
        opts,
        progress,
        cancel,
        started,
    )
    .await?;
    if opts.mark_of_the_web {
        crate::mark_of_the_web(&req.output, &req.manifest, &req.headers);
    }
    Ok(outcome)
}
