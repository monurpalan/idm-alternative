//! HLS: downloads the segments of a stream's playlists in parallel, decrypts AES-128, joins
//! them, and (with ffmpeg) puts video and audio into one file without re-encoding.
//!
//! Segments are written to `<output>.idma-hls/` as they complete (`.part` first, renamed when
//! whole), so a stopped download continues with the segments that are missing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

use crate::download::{Options, Outcome, Phase, Progress, http_config};
use crate::error::{Error, Result};
use crate::remote::Http;
use crate::state::sibling;

/// What to download: a media playlist (or a master one, whose best variant is taken), an
/// optional separate audio playlist, and where the result goes.
#[derive(Debug, Clone)]
pub struct StreamRequest {
    pub video: String,
    pub audio: Option<String>,
    pub output: PathBuf,
    pub headers: Vec<(String, String)>,
    /// Joins the tracks into `output`; without it a single track's segments are joined as
    /// they are (`.ts`), and a separate audio track is an error.
    pub ffmpeg: Option<PathBuf>,
}

/// Playlists larger than this are not playlists.
const MAX_PLAYLIST: usize = 16 << 20;
/// Segments fetched at once when `connections` allows more.
const MAX_PARALLEL: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Segment {
    pub url: String,
    /// `[start, end)` within `url` (`EXT-X-BYTERANGE`).
    pub range: Option<(u64, u64)>,
    pub key: Option<Key>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Key {
    pub url: String,
    pub iv: [u8; 16],
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MediaPlaylist {
    /// fMP4 initialisation segment (`EXT-X-MAP`), joined first.
    pub init: Option<Segment>,
    pub segments: Vec<Segment>,
}

/// What a playlist is: segments, or (a master playlist) the best variant to follow.
#[derive(Debug, PartialEq)]
pub(crate) enum Playlist {
    Media(MediaPlaylist),
    Best {
        video: String,
        audio: Option<String>,
    },
}

fn attr<'a>(list: &'a str, key: &str) -> Option<&'a str> {
    // Good enough for the attributes read here (no commas inside the quoted ones we need).
    let mut rest = list;
    while let Some(eq) = rest.find('=') {
        let k = rest[..eq].trim().trim_start_matches(',').trim();
        let after = &rest[eq + 1..];
        let (value, next) = if let Some(q) = after.strip_prefix('"') {
            let end = q.find('"').unwrap_or(q.len());
            (&q[..end], q.get(end + 1..).unwrap_or(""))
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (&after[..end], &after[end..])
        };
        if k.eq_ignore_ascii_case(key) {
            return Some(value);
        }
        rest = next;
    }
    None
}

fn resolve(base: &str, rel: &str) -> String {
    url::Url::parse(base)
        .and_then(|b| b.join(rel))
        .map_or_else(|_| rel.to_owned(), |u| u.to_string())
}

/// `0x1234…` → 16 bytes, right-aligned.
fn hex_iv(s: &str) -> Option<[u8; 16]> {
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    let n = u128::from_str_radix(hex, 16).ok()?;
    Some(n.to_be_bytes())
}

pub(crate) fn parse_playlist(url: &str, text: &str) -> Result<Playlist> {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim().trim_start_matches('\u{feff}'))
        .filter(|l| !l.is_empty())
        .collect();
    if lines.first() != Some(&"#EXTM3U") {
        return Err(Error::State(format!("{url} is not an HLS playlist")));
    }

    // Master playlist: the variant with the most pixels, then bandwidth.
    let mut best: Option<(u64, u64, String, Option<String>)> = None;
    let mut audio_groups: HashMap<String, String> = HashMap::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(a) = line.strip_prefix("#EXT-X-MEDIA:")
            && attr(a, "TYPE") == Some("AUDIO")
            && let (Some(group), Some(uri)) = (attr(a, "GROUP-ID"), attr(a, "URI"))
        {
            let default = attr(a, "DEFAULT") == Some("YES");
            if default || !audio_groups.contains_key(group) {
                audio_groups.insert(group.to_owned(), resolve(url, uri));
            }
        }
        if let Some(a) = line.strip_prefix("#EXT-X-STREAM-INF:")
            && let Some(uri) = lines[i + 1..].iter().find(|l| !l.starts_with('#'))
        {
            let pixels = attr(a, "RESOLUTION")
                .and_then(|r| r.split_once('x'))
                .and_then(|(w, h)| Some(w.parse::<u64>().ok()? * h.parse::<u64>().ok()?))
                .unwrap_or(0);
            let bw = attr(a, "BANDWIDTH")
                .and_then(|b| b.parse().ok())
                .unwrap_or(0);
            if best
                .as_ref()
                .is_none_or(|(p, b, ..)| (pixels, bw) > (*p, *b))
            {
                best = Some((
                    pixels,
                    bw,
                    resolve(url, uri),
                    attr(a, "AUDIO").map(str::to_owned),
                ));
            }
        }
    }
    if let Some((_, _, video, group)) = best {
        let audio = group.and_then(|g| audio_groups.get(&g).cloned());
        return Ok(Playlist::Best { video, audio });
    }

    let mut segments = Vec::new();
    let mut init = None;
    let mut key: Option<(String, Option<[u8; 16]>)> = None;
    let mut sequence: u64 = 0;
    let mut range: Option<(u64, u64)> = None;
    let mut last_end: HashMap<String, u64> = HashMap::new();
    let mut ended = false;
    let mut in_segment = false;
    for line in &lines {
        if let Some(n) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            sequence = n.trim().parse().unwrap_or(0);
        } else if let Some(a) = line.strip_prefix("#EXT-X-KEY:") {
            key = match attr(a, "METHOD") {
                None | Some("NONE") => None,
                Some("AES-128") => {
                    let uri = attr(a, "URI")
                        .ok_or_else(|| Error::State("AES-128 key without URI".into()))?;
                    Some((resolve(url, uri), attr(a, "IV").and_then(hex_iv)))
                }
                Some(other) => {
                    return Err(Error::State(format!(
                        "segments encrypted with {other} cannot be downloaded"
                    )));
                }
            };
        } else if let Some(a) = line.strip_prefix("#EXT-X-MAP:") {
            let uri = attr(a, "URI").ok_or_else(|| Error::State("EXT-X-MAP without URI".into()))?;
            let range = attr(a, "BYTERANGE").and_then(|r| byte_range(r, 0));
            init = Some(Segment {
                url: resolve(url, uri),
                range,
                key: None,
            });
        } else if let Some(r) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            // (offset, length); an offset of MAX means "where the previous range of this
            // URI ended", resolved once the URI is known.
            range = parse_range_spec(r).map(|(len, off)| (off.unwrap_or(u64::MAX), len));
        } else if line.starts_with("#EXTINF:") {
            in_segment = true;
        } else if *line == "#EXT-X-ENDLIST" {
            ended = true;
        } else if !line.starts_with('#') && in_segment {
            let seg_url = resolve(url, line);
            let byte_range = range.take().map(|(off, len)| {
                let start = if off == u64::MAX {
                    *last_end.get(&seg_url).unwrap_or(&0)
                } else {
                    off
                };
                (start, start + len)
            });
            if let Some((_, end)) = byte_range {
                last_end.insert(seg_url.clone(), end);
            }
            let k = key.as_ref().map(|(u, iv)| Key {
                url: u.clone(),
                // Without an IV attribute, the segment's sequence number (RFC 8216 §5.2).
                iv: iv.unwrap_or_else(|| u128::from(sequence).to_be_bytes()),
            });
            segments.push(Segment {
                url: seg_url,
                range: byte_range,
                key: k,
            });
            sequence += 1;
            in_segment = false;
        }
    }
    if segments.is_empty() {
        return Err(Error::State("the playlist has no segments".into()));
    }
    if !ended {
        return Err(Error::State(
            "live streams cannot be downloaded yet (the playlist has no end)".into(),
        ));
    }
    Ok(Playlist::Media(MediaPlaylist { init, segments }))
}

/// `length[@offset]`.
fn parse_range_spec(r: &str) -> Option<(u64, Option<u64>)> {
    let (len, off) = match r.trim().split_once('@') {
        Some((l, o)) => (l, Some(o.parse().ok()?)),
        None => (r.trim(), None),
    };
    Some((len.parse().ok()?, off))
}

fn byte_range(r: &str, default_start: u64) -> Option<(u64, u64)> {
    let (len, off) = parse_range_spec(r)?;
    let start = off.unwrap_or(default_start);
    Some((start, start + len))
}

/// Fetches a playlist and follows a master playlist to its best variant.
async fn media_playlist(http: &Http, url: &str) -> Result<(MediaPlaylist, Option<String>)> {
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
struct Run {
    http: Http,
    dir: PathBuf,
    keys: Mutex<HashMap<String, [u8; 16]>>,
    received: AtomicU64,
    done: AtomicUsize,
    active: AtomicUsize,
    opts: Options,
    cancel: CancellationToken,
}

impl Run {
    async fn key(&self, url: &str) -> Result<[u8; 16]> {
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
    async fn fetch(&self, seg: &Segment, path: &Path, slot: u64) -> Result<()> {
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

    async fn try_fetch(&self, seg: &Segment, path: &Path, slot: u64) -> Result<()> {
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
fn track_files(dir: &Path, track: &str, playlist: &MediaPlaylist) -> Vec<(Segment, PathBuf)> {
    playlist
        .init
        .iter()
        .map(|s| (s.clone(), dir.join(format!("{track}-init.seg"))))
        .chain(
            playlist
                .segments
                .iter()
                .enumerate()
                .map(|(i, s)| (s.clone(), dir.join(format!("{track}-{i:06}.seg")))),
        )
        .collect()
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

    let dir = sibling(&req.output, ".idma-hls");
    tokio::fs::create_dir_all(&dir).await?;
    let mut files = track_files(&dir, "video", &video);
    if let Some(a) = &audio {
        files.extend(track_files(&dir, "audio", a));
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

    let reporter = progress.map(|tx| {
        let tx = tx.clone();
        let run = Arc::clone(&run);
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
    });

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

    // Join each track, then put them together.
    let video_path = dir.join("video.joined");
    join(&track_files(&dir, "video", &video), &video_path).await?;
    let audio_path = match &audio {
        Some(a) => {
            let p = dir.join("audio.joined");
            join(&track_files(&dir, "audio", a), &p).await?;
            Some(p)
        }
        None => None,
    };
    match &req.ffmpeg {
        Some(ffmpeg) => {
            if let Some(p) = progress {
                p.send_modify(|p| p.phase = Phase::Verifying);
            }
            mux(ffmpeg, &video_path, audio_path.as_deref(), &req.output).await?;
        }
        None => tokio::fs::rename(&video_path, &req.output).await?,
    }
    tokio::fs::remove_dir_all(&run.dir).await.ok();
    let size = tokio::fs::metadata(&req.output).await?.len();
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
    })
}

async fn join(files: &[(Segment, PathBuf)], out: &Path) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut f = tokio::fs::File::create(out).await?;
    for (_, path) in files {
        let mut src = tokio::fs::File::open(path).await?;
        tokio::io::copy(&mut src, &mut f).await?;
    }
    f.flush().await?;
    Ok(())
}

/// Puts the joined tracks into `out` without re-encoding.
async fn mux(ffmpeg: &Path, video: &Path, audio: Option<&Path>, out: &Path) -> Result<()> {
    let mut cmd = tokio::process::Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(video);
    if let Some(a) = audio {
        cmd.arg("-i")
            .arg(a)
            .args(["-map", "0:v:0?", "-map", "1:a:0?"]);
    } else {
        cmd.args(["-map", "0"]);
    }
    // ADTS AAC from MPEG-TS needs its headers rewritten for MP4.
    cmd.args(["-c", "copy", "-bsf:a", "aac_adtstoasc"]).arg(out);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_picks_the_best_variant_and_its_audio() {
        let text = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"x\",URI=\"a1.m3u8\"\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"y\",DEFAULT=YES,URI=\"a2.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=9,RESOLUTION=640x360,AUDIO=\"a\"\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=5,RESOLUTION=1920x1080,AUDIO=\"a\"\nhigh.m3u8\n";
        assert_eq!(
            parse_playlist("https://h/x/m.m3u8", text).expect("parse"),
            Playlist::Best {
                video: "https://h/x/high.m3u8".into(),
                audio: Some("https://h/x/a2.m3u8".into()),
            }
        );
    }

    #[test]
    fn media_playlist_with_keys_ranges_and_init() {
        let text = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"100@0\"\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:4,\n#EXT-X-BYTERANGE:1000@100\nall.mp4\n#EXTINF:4,\n#EXT-X-BYTERANGE:500\nall.mp4\n#EXT-X-KEY:METHOD=AES-128,URI=\"k2\",IV=0x0000000000000000000000000000ABCD\n#EXTINF:4,\nlast.ts\n#EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse_playlist("https://h/v/p.m3u8", text).expect("parse") else {
            panic!("not media");
        };
        assert_eq!(m.init.as_ref().map(|i| i.range), Some(Some((0, 100))));
        assert_eq!(m.segments.len(), 3);
        assert_eq!(m.segments[0].range, Some((100, 1100)));
        assert_eq!(
            m.segments[1].range,
            Some((1100, 1600)),
            "continues the previous range"
        );
        assert_eq!(m.segments[0].key.as_ref().map(|k| k.iv[15]), Some(7));
        assert_eq!(m.segments[1].key.as_ref().map(|k| k.iv[15]), Some(8));
        let last = m.segments[2].key.as_ref().expect("key");
        assert_eq!(last.url, "https://h/v/k2");
        assert_eq!(&last.iv[14..], &[0xAB, 0xCD]);
        assert_eq!(m.segments[2].range, None);
    }

    #[test]
    fn refuses_live_and_drm() {
        assert!(
            parse_playlist("u", "#EXTM3U\n#EXTINF:4,\na.ts\n").is_err(),
            "live"
        );
        assert!(
            parse_playlist(
                "u",
                "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\"\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n"
            )
            .is_err()
        );
        assert!(parse_playlist("u", "nope").is_err());
    }
}
