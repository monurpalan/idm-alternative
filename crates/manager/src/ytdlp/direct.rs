//! A video page's formats downloaded by the engine rather than by yt-dlp: yt-dlp only says
//! where they are (`-J`), then video and audio come at the same time, each over several
//! connections, and ffmpeg joins them as yt-dlp would. yt-dlp fetches one after the other
//! over one connection. Whatever the engine cannot fetch straight from an address (a stream
//! in fragments, a live broadcast, a link that stops working) goes back to yt-dlp.

use super::*;

use shardload_engine::{Options, Request, SegmentView};

/// One format yt-dlp chose, fetched straight from its address.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectFormat {
    pub id: String,
    pub url: String,
    /// The headers yt-dlp would send for it.
    pub headers: Vec<(String, String)>,
    pub ext: String,
    /// What yt-dlp says it weighs, for the progress before the server answers.
    pub size: Option<u64>,
    /// The largest request the site serves at full speed (yt-dlp's
    /// `downloader_options.http_chunk_size`): YouTube serves a larger one slowly.
    pub chunk: Option<u64>,
    /// When the address starts working, in seconds since 1970: YouTube refuses one for a
    /// few seconds after handing it out (yt-dlp waits too).
    pub available_at: Option<u64>,
}

/// The longest wait for an address to start working; a longer one is left to yt-dlp.
const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// How a download through the engine ended.
#[derive(Debug)]
pub enum Direct {
    /// The file written.
    Done(PathBuf),
    Cancelled,
    /// yt-dlp has to download it, for this reason.
    Fallback(String),
}

/// The formats chosen in `yt-dlp -J -f …` output, when the engine can fetch each straight
/// from its address: at most two (video and audio), each one whole file over HTTP(S),
/// without DRM, and not a live broadcast.
pub fn direct_formats(v: &serde_json::Value) -> Result<Vec<DirectFormat>, String> {
    let live = v.get("is_live").and_then(serde_json::Value::as_bool) == Some(true)
        || matches!(
            v.get("live_status").and_then(serde_json::Value::as_str),
            Some("is_live" | "is_upcoming" | "post_live")
        );
    if live {
        return Err("a live broadcast".into());
    }
    let chosen: Vec<&serde_json::Value> = match v
        .get("requested_formats")
        .and_then(serde_json::Value::as_array)
    {
        Some(f) if !f.is_empty() => f.iter().collect(),
        _ => vec![v],
    };
    if chosen.len() > 2 {
        return Err(format!("{} formats to join", chosen.len()));
    }
    chosen.into_iter().map(direct_format).collect()
}

fn direct_format(f: &serde_json::Value) -> Result<DirectFormat, String> {
    let text = |k: &str| f.get(k).and_then(serde_json::Value::as_str);
    let id = text("format_id").unwrap_or("0").to_owned();
    let protocol = text("protocol").unwrap_or_default();
    if !matches!(protocol, "https" | "http") {
        return Err(format!("format {id} comes as {protocol:?}"));
    }
    let url = text("url")
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .ok_or_else(|| format!("format {id} has no address"))?;
    if f.get("fragments").is_some() || f.get("fragment_base_url").is_some() {
        return Err(format!("format {id} comes in fragments"));
    }
    if f.get("has_drm").and_then(serde_json::Value::as_bool) == Some(true) {
        return Err(format!("format {id} is DRM-protected"));
    }
    let headers = f
        .get("http_headers")
        .and_then(serde_json::Value::as_object)
        .map(|h| {
            h.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    let number = |k: &str| f.get(k).and_then(serde_json::Value::as_u64);
    Ok(DirectFormat {
        id: id.clone(),
        url: url.to_owned(),
        headers,
        ext: text("ext").unwrap_or("mp4").to_owned(),
        size: number("filesize").or_else(|| number("filesize_approx")),
        chunk: f
            .pointer("/downloader_options/http_chunk_size")
            .and_then(serde_json::Value::as_u64)
            .filter(|&c| c > 0),
        available_at: f
            .get("available_at")
            .and_then(serde_json::Value::as_f64)
            .map(|t| t.ceil() as u64),
    })
}

/// Asks yt-dlp where the formats of `format` are, without downloading them.
pub async fn resolve(
    tools: &Tools,
    url: &str,
    format: &str,
    output_stem: &Path,
    headers: &[(String, String)],
) -> Result<Vec<DirectFormat>, String> {
    let cookie_path = PathBuf::from(format!("{}.shardload-cookies.txt", output_stem.display()));
    let cookies = write_cookies(&cookie_path, url, headers);
    let mut args = vec![
        "-J".to_owned(),
        "-f".into(),
        prefer_playable(format),
        "-S".into(),
        FORMAT_SORT.into(),
    ];
    args.extend(base_args(tools, url, headers, cookies.as_deref()));
    let out = command(&tools.ytdlp).args(&args).output().await;
    if let Some(c) = cookies {
        std::fs::remove_file(c).ok();
    }
    let out = out.map_err(|e| format!("could not run yt-dlp: {e}"))?;
    if !out.status.success() {
        return Err(error_text(&out.stderr));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("yt-dlp answered something unexpected: {e}"))?;
    direct_formats(&v)
}

/// Where each format goes: one alone straight to `<stem>.<ext>`; two side by side as yt-dlp
/// names them (`<stem>.f<id>.<ext>`), joined later into `<stem>.mp4`.
pub fn part_paths(formats: &[DirectFormat], output_stem: &Path) -> Vec<PathBuf> {
    let stem = output_stem.display();
    match formats {
        [one] => vec![PathBuf::from(format!("{stem}.{}", one.ext))],
        _ => formats
            .iter()
            .map(|f| {
                let id =
                    f.id.chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                        .collect::<String>();
                PathBuf::from(format!("{stem}.f{id}.{}", f.ext))
            })
            .collect(),
    }
}

/// `connections` shared by the formats by their sizes, at least one each.
fn share(connections: usize, formats: &[DirectFormat]) -> Vec<usize> {
    let connections = connections.max(formats.len());
    let sizes: Vec<u64> = formats.iter().map(|f| f.size.unwrap_or(1).max(1)).collect();
    let total: u64 = sizes.iter().sum();
    let mut shares: Vec<usize> = sizes
        .iter()
        .map(|&s| ((connections as u64 * s).div_ceil(total) as usize).max(1))
        .collect();
    // Rounding up may hand out one too many: the largest gives it back.
    while shares.iter().sum::<usize>() > connections {
        let i = (0..shares.len()).max_by_key(|&i| shares[i]).unwrap_or(0);
        shares[i] -= 1;
    }
    shares
}

/// The progress of downloads side by side, as one: their sizes and speeds added, the
/// second's segments after the first's.
fn combine(parts: &[(Progress, Option<u64>)]) -> Progress {
    let mut all = Progress {
        phase: Phase::Downloading,
        size: Some(0),
        ..Progress::default()
    };
    let mut offset = 0;
    for (p, estimate) in parts {
        let size = p.size.or(*estimate);
        all.size = match (all.size, size) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };
        all.received += p.received;
        all.speed += p.speed;
        all.connections += p.connections;
        all.segments.extend(p.segments.iter().map(|s| SegmentView {
            start: s.start + offset,
            end: s.end.saturating_add(offset),
            ..s.clone()
        }));
        offset += size.unwrap_or(p.received);
        if all.health == Default::default() {
            all.health = p.health.clone();
        }
    }
    all
}

/// Downloads every format to its path at the same time; the first error stops the others.
async fn fetch(
    formats: &[DirectFormat],
    paths: &[PathBuf],
    opts: &Options,
    progress: &watch::Sender<Progress>,
    cancel: &CancellationToken,
) -> Result<(), shardload_engine::Error> {
    let shares = share(opts.connections, formats);
    let stop = cancel.child_token();
    let (senders, receivers): (Vec<_>, Vec<_>) = formats
        .iter()
        .map(|_| watch::channel(Progress::default()))
        .unzip();
    let runs = formats.iter().zip(paths).zip(&senders).zip(shares).map(
        |(((f, path), tx), connections)| {
            let stop = stop.clone();
            let mut opts = opts.clone();
            opts.connections = connections;
            opts.max_request = f.chunk;
            // The joined file is checked and marked; its parts are not worth a search.
            opts.find_checksum = false;
            opts.mark_of_the_web = false;
            opts.checksum = None;
            opts.page_checksum = None;
            opts.pieces = None;
            opts.learn = shardload_engine::Learning::default();
            let req = Request {
                url: f.url.clone(),
                output: path.clone(),
                headers: f.headers.clone(),
                mirrors: Vec::new(),
            };
            let size = f.size;
            async move {
                // Finished on an earlier start, before the join.
                let part = PathBuf::from(format!("{}.shardload-part", path.display()));
                let whole =
                    std::fs::metadata(path).is_ok_and(|m| size.is_none_or(|s| m.len() == s));
                if whole && !part.exists() {
                    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                    tx.send_modify(|p| {
                        p.size = Some(len);
                        p.received = len;
                    });
                    return Ok(());
                }
                let r = shardload_engine::download(&req, &opts, Some(tx), &stop).await;
                if r.is_err() {
                    stop.cancel();
                }
                r.map(|_| ())
            }
        },
    );
    let work = futures_util::future::join_all(runs);
    tokio::pin!(work);
    let estimates: Vec<Option<u64>> = formats.iter().map(|f| f.size).collect();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
    let results = loop {
        tokio::select! {
            r = &mut work => break r,
            _ = tick.tick() => {
                let parts: Vec<(Progress, Option<u64>)> = receivers
                    .iter()
                    .zip(&estimates)
                    .map(|(rx, e)| (rx.borrow().clone(), *e))
                    .collect();
                progress.send_replace(combine(&parts));
            }
        }
    };
    // A real failure rather than the cancel it caused in the others.
    let mut first_cancel = None;
    for r in results {
        match r {
            Err(shardload_engine::Error::Cancelled) => first_cancel = Some(()),
            Err(e) => return Err(e),
            Ok(()) => {}
        }
    }
    match first_cancel {
        Some(()) => Err(shardload_engine::Error::Cancelled),
        None => Ok(()),
    }
}

/// Joins a video and an audio file into the MP4 `out` without re-encoding, as yt-dlp does.
pub async fn merge(ffmpeg: &Path, parts: &[PathBuf], out: &Path) -> Result<(), String> {
    let tmp = out.with_extension("temp.mp4");
    let mut cmd = command(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    for p in parts {
        cmd.arg("-i").arg(p);
    }
    for i in 0..parts.len() {
        cmd.args(["-map", &i.to_string()]);
    }
    cmd.args(["-c", "copy", "-movflags", "+faststart"])
        .arg(&tmp);
    let out_ = cmd
        .output()
        .await
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;
    if !out_.status.success() {
        std::fs::remove_file(&tmp).ok();
        return Err(error_text(&out_.stderr));
    }
    std::fs::rename(&tmp, out).map_err(|e| e.to_string())
}

/// Removes what the engine left of `paths`: partial files, their state, finished parts.
pub fn discard_parts(paths: &[PathBuf]) {
    for p in paths {
        for suffix in ["", ".shardload-part", ".shardload-state", ".shardload-lock"] {
            std::fs::remove_file(format!("{}{suffix}", p.display())).ok();
        }
    }
}

/// Returns once `format`'s address works, `wait` from now at the latest: it often works a
/// second or two before the time the site gives, so it is tried from a little earlier.
async fn ready(format: &DirectFormat, opts: &Options, wait: std::time::Duration) {
    const EARLY: std::time::Duration = std::time::Duration::from_secs(3);
    const EVERY: std::time::Duration = std::time::Duration::from_millis(250);
    let deadline = tokio::time::Instant::now() + wait;
    tokio::time::sleep(wait.saturating_sub(EARLY)).await;
    let mut opts = opts.clone();
    opts.connections = 1;
    while tokio::time::Instant::now() + EVERY < deadline {
        if shardload_engine::probe(&format.url, &format.headers, &opts)
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(EVERY).await;
    }
    tokio::time::sleep_until(deadline).await;
}

/// Waits until the formats' addresses work (asked for before, they are refused); the
/// error is what to do instead.
async fn wait_until_ready(
    formats: &[DirectFormat],
    opts: &Options,
    cancel: &CancellationToken,
) -> Result<(), Direct> {
    let wait = until(formats.iter().filter_map(|f| f.available_at).max());
    if wait > MAX_WAIT {
        return Err(Direct::Fallback(format!(
            "its address works only in {wait:?}"
        )));
    }
    tokio::select! {
        () = ready(&formats[0], opts, wait) => Ok(()),
        () = cancel.cancelled() => Err(Direct::Cancelled),
    }
}

/// How long until `at` (seconds since 1970), if it is still to come.
fn until(at: Option<u64>) -> std::time::Duration {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    at.map(std::time::Duration::from_secs)
        .and_then(|at| at.checked_sub(now))
        .unwrap_or_default()
}

/// Downloads `url` in `format` through the engine to `<stem>.mp4` (or `<stem>.<ext>` for a
/// single format), video and audio at the same time, sharing `opts.connections`. A pause
/// keeps the partial files; the next start asks yt-dlp for fresh addresses and goes on from
/// them. A link that fails part way is asked for again once; whatever cannot be done this
/// way is left to yt-dlp ([`Direct::Fallback`]), with what was downloaded removed.
#[allow(clippy::too_many_arguments)]
pub async fn download_direct(
    tools: &Tools,
    url: &str,
    format: &str,
    output_stem: &Path,
    headers: &[(String, String)],
    opts: &Options,
    progress: &watch::Sender<Progress>,
    cancel: &CancellationToken,
) -> Direct {
    let mut tried_again = false;
    let mut paths: Vec<PathBuf> = Vec::new();
    loop {
        let resolved = tokio::select! {
            r = resolve(tools, url, format, output_stem, headers) => r,
            () = cancel.cancelled() => return Direct::Cancelled,
        };
        let formats = match resolved {
            Ok(f) => f,
            Err(why) => {
                discard_parts(&paths);
                return Direct::Fallback(why);
            }
        };
        if formats.len() > 1 && tools.ffmpeg.is_none() {
            return Direct::Fallback("no ffmpeg to join video and audio".into());
        }
        let fresh = part_paths(&formats, output_stem);
        if !paths.is_empty() && fresh != paths {
            // Other formats this time: what the first try left is of no use.
            discard_parts(&paths);
        }
        paths = fresh;
        if let Err(stopped) = wait_until_ready(&formats, opts, cancel).await {
            return stopped;
        }
        progress.send_modify(|p| p.phase = Phase::Downloading);
        match fetch(&formats, &paths, opts, progress, cancel).await {
            Ok(()) => break,
            Err(_) if cancel.is_cancelled() => return Direct::Cancelled,
            Err(e) if !tried_again => {
                tracing::info!("engine download of a video page failed ({e}), asking again");
                tried_again = true;
            }
            Err(e) => {
                discard_parts(&paths);
                return Direct::Fallback(e.to_string());
            }
        }
    }
    if paths.len() == 1 {
        return Direct::Done(paths.swap_remove(0));
    }
    let ffmpeg = tools.ffmpeg.as_deref().expect("checked above");
    join(ffmpeg, &paths, output_stem, cancel).await
}

/// Joins the downloaded parts into `<stem>.mp4`, removing them.
async fn join(
    ffmpeg: &Path,
    paths: &[PathBuf],
    output_stem: &Path,
    cancel: &CancellationToken,
) -> Direct {
    let out = PathBuf::from(format!("{}.mp4", output_stem.display()));
    let joined = tokio::select! {
        r = merge(ffmpeg, paths, &out) => r,
        () = cancel.cancelled() => return Direct::Cancelled,
    };
    match joined {
        Ok(()) => {
            discard_parts(paths);
            Direct::Done(out)
        }
        Err(why) => {
            discard_parts(paths);
            Direct::Fallback(format!("ffmpeg could not join video and audio: {why}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(id: &str, size: u64) -> serde_json::Value {
        serde_json::json!({
            "format_id": id, "protocol": "https", "ext": "webm", "filesize": size,
            "url": format!("https://example.com/{id}"),
            "http_headers": {"User-Agent": "UA"},
            "downloader_options": {"http_chunk_size": 10485760},
            "available_at": 1_794_053_797.4,
        })
    }

    #[test]
    fn video_and_audio_with_addresses_are_fetched_directly() {
        let v = serde_json::json!({"requested_formats": [format("271", 900), format("140", 100)]});
        let f = direct_formats(&v).expect("direct");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].url, "https://example.com/271");
        assert_eq!(f[0].headers, [("User-Agent".to_owned(), "UA".to_owned())]);
        assert_eq!(f[0].chunk, Some(10 << 20));
        assert_eq!(f[1].size, Some(100));
        assert_eq!(f[1].available_at, Some(1_794_053_798));
        let paths = part_paths(&f, Path::new("dir/v"));
        assert_eq!(paths[0], PathBuf::from("dir/v.f271.webm"));
        // One format alone, at the top level: straight to its own name.
        let one = direct_formats(&format("18", 5)).expect("direct");
        assert_eq!(
            part_paths(&one, Path::new("dir/v")),
            [PathBuf::from("dir/v.webm")]
        );
    }

    #[test]
    fn what_the_engine_cannot_fetch_is_left_to_yt_dlp() {
        let mut hls = format("95", 1);
        hls["protocol"] = "m3u8_native".into();
        let mut no_url = format("137", 1);
        no_url.as_object_mut().expect("an object").remove("url");
        let mut fragments = format("137", 1);
        fragments["fragments"] = serde_json::json!([]);
        let mut drm = format("137", 1);
        drm["has_drm"] = true.into();
        for bad in [hls, no_url, fragments, drm] {
            let v = serde_json::json!({"requested_formats": [bad, format("140", 1)]});
            assert!(direct_formats(&v).is_err(), "{v}");
        }
        let mut live = format("18", 1);
        live["live_status"] = "is_live".into();
        assert!(direct_formats(&live).is_err());
    }

    #[test]
    fn connections_are_shared_by_size() {
        let f = |size| DirectFormat {
            id: String::new(),
            url: String::new(),
            headers: Vec::new(),
            ext: String::new(),
            size: Some(size),
            chunk: None,
            available_at: None,
        };
        assert_eq!(share(16, &[f(224), f(14)]), [15, 1]);
        assert_eq!(share(1, &[f(224), f(14)]), [1, 1]);
        assert_eq!(share(8, &[f(10)]), [8]);
        assert_eq!(share(4, &[f(50), f(50)]), [2, 2]);
    }

    #[test]
    fn progress_side_by_side_adds_up() {
        let p = |size, received, speed, conns| Progress {
            size: Some(size),
            received,
            speed,
            connections: conns,
            segments: vec![SegmentView {
                start: 0,
                end: size,
                ..SegmentView::default()
            }],
            ..Progress::default()
        };
        let all = combine(&[(p(100, 40, 5.0, 3), None), (p(10, 10, 1.0, 1), None)]);
        assert_eq!(all.size, Some(110));
        assert_eq!(all.received, 50);
        assert_eq!(all.connections, 4);
        assert_eq!(all.segments[1].start, 100);
        // Before the server answers, yt-dlp's estimate stands in for the size.
        let early = combine(&[
            (Progress::default(), Some(100)),
            (Progress::default(), Some(10)),
        ]);
        assert_eq!(early.size, Some(110));
    }
}
