//! Running yt-dlp for a download: its progress, stopping it with what it started, and
//! finding the file it wrote.

use super::*;

/// Fragments fetched at once at most, whatever the connection setting: enough for a
/// stream's fragments, few enough not to look like an attack to the site.
pub const MAX_FRAGMENTS: usize = 8;

/// One progress line from our `--progress-template`: downloaded, total, estimated total,
/// speed (`NA` when unknown).
pub(crate) fn parse_progress(line: &str) -> Option<(u64, Option<u64>, f64)> {
    let rest = line.strip_prefix("shardload ")?;
    let mut it = rest.split_whitespace();
    let num = |s: Option<&str>| s.and_then(|s| s.parse::<f64>().ok());
    let downloaded = num(it.next())? as u64;
    let total = num(it.next()).or(num(it.next())).map(|t| t as u64);
    let speed = num(it.next()).unwrap_or(0.0);
    Some((downloaded, total, speed))
}

/// Downloads `url` in `format` to `<dir>/<stem>.<ext>` and returns the file written. yt-dlp
/// downloads video and audio one after the other and joins them; progress covers both.
/// With `clip`, only that part, cut at keyframes forced at its ends (needs ffmpeg).
/// Cancelling stops yt-dlp; running again continues its partial files. A live broadcast
/// (`record`) is recorded as MPEG-TS until cancelled, which then keeps what was recorded.
/// A stream sent in fragments (HLS, DASH) is fetched over up to `connections` of them at
/// once, at most [`MAX_FRAGMENTS`]; one file sent whole (most of YouTube) is not affected.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    tools: &Tools,
    url: &str,
    format: &str,
    clip: Option<&crate::clip::Clip>,
    record: Option<&crate::task::Recording>,
    output_stem: &Path,
    headers: &[(String, String)],
    connections: usize,
    progress: &watch::Sender<Progress>,
    cancel: &CancellationToken,
) -> Result<PathBuf, String> {
    let cookie_path = PathBuf::from(format!("{}.shardload-cookies.txt", output_stem.display()));
    let cookies = write_cookies(&cookie_path, url, headers);
    let mut args = vec![
        "-f".to_owned(),
        format.to_owned(),
        "-S".into(),
        FORMAT_SORT.into(),
        "--merge-output-format".into(),
        "mp4".into(),
        "--continue".into(),
        "--newline".into(),
        "--progress-template".into(),
        "download:shardload %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s %(progress.speed)s".into(),
        "--print".into(),
        "after_move:shardload-file %(filepath)s".into(),
        "-o".into(),
        format!("{}.%(ext)s", output_stem.display()),
    ];
    if let Some(c) = clip {
        args.extend([
            "--download-sections".into(),
            crate::clip::section(c),
            "--force-keyframes-at-cuts".into(),
        ]);
    }
    if let Some(r) = record {
        // Written as it comes, in a container that stays playable when cut off.
        args.extend(["--hls-use-mpegts".into(), "--no-part".into()]);
        if r.from_start {
            args.push("--live-from-start".into());
        }
    } else {
        let n = connections.clamp(1, MAX_FRAGMENTS);
        args.extend(["--concurrent-fragments".into(), n.to_string()]);
    }
    args.extend(base_args(tools, url, headers, cookies.as_deref()));
    let mut cmd = command(&tools.ytdlp);
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so stopping it stops ffmpeg too (see `kill_tree`).
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not run yt-dlp: {e}"))?;
    let stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let errors = tokio::spawn(async move {
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut buf)
            .await
            .ok();
        buf
    });

    // Split on bytes and decode leniently: one line in another encoding must not end the read.
    let mut lines = BufReader::new(stdout).split(b'\n');
    let mut file: Option<PathBuf> = None;
    // Bytes of the files already finished (video before audio), and the last count.
    let mut finished = 0u64;
    let mut last = (0u64, None::<u64>);
    progress.send_modify(|p| p.phase = Phase::Downloading);
    let result = loop {
        tokio::select! {
            line = lines.next_segment() => match line {
                Ok(Some(line)) => {
                    let line = String::from_utf8_lossy(&line);
                    let line = line.trim_end_matches('\r');
                    if let Some(path) = line.strip_prefix("shardload-file ") {
                        file = Some(PathBuf::from(path.trim()));
                    } else if let Some((got, total, speed)) = parse_progress(line) {
                        if got < last.0 {
                            // A new file started.
                            finished += last.1.unwrap_or(last.0);
                        }
                        last = (got, total);
                        progress.send_modify(|p| {
                            p.phase = Phase::Downloading;
                            p.received = finished + got;
                            p.size = total.map(|t| finished + t);
                            p.speed = speed;
                            p.connections = 1;
                        });
                    }
                }
                Ok(None) => break child.wait().await.map_err(|e| e.to_string()),
                Err(e) => break Err(e.to_string()),
            },
            () = cancel.cancelled() => {
                kill_tree(&mut child).await;
                if let Some(c) = &cookies {
                    std::fs::remove_file(c).ok();
                }
                // A recording that was stopped: what it wrote so far.
                if record.is_some() {
                    return file
                        .filter(|f| f.is_file())
                        .or_else(|| written_file(output_stem))
                        .filter(|f| std::fs::metadata(f).is_ok_and(|m| m.len() > 0))
                        .ok_or_else(|| "nothing was recorded".to_owned());
                }
                return Err("cancelled".into());
            }
        }
    };
    if let Some(c) = &cookies {
        std::fs::remove_file(c).ok();
    }
    let status = result?;
    let stderr = errors.await.unwrap_or_default();
    if !status.success() {
        return Err(error_text(&stderr));
    }
    file.filter(|f| f.is_file())
        .or_else(|| written_file(output_stem))
        .ok_or_else(|| "yt-dlp finished without saying which file it wrote".to_owned())
}

/// Stops yt-dlp and what it started (ffmpeg, which downloads a live stream or joins tracks):
/// killing yt-dlp alone leaves them running.
pub(crate) async fn kill_tree(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        #[cfg(windows)]
        {
            command(Path::new("taskkill"))
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .output()
                .await
                .ok();
        }
        #[cfg(unix)]
        {
            // yt-dlp leads its own process group (see `run`).
            command(Path::new("kill"))
                .args(["-TERM", &format!("-{pid}")])
                .output()
                .await
                .ok();
        }
    }
    child.kill().await.ok();
}

/// Copies a recording (MPEG-TS, whatever its name) into an MP4 at `out`, without
/// re-encoding, and deletes the original. Returns the MP4.
pub async fn remux(ffmpeg: &Path, recording: &Path, out: &Path) -> Result<PathBuf, String> {
    let tmp = out.with_extension("remux.mp4");
    let status = command(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(recording)
        .args(["-map", "0", "-c", "copy", "-bsf:a", "aac_adtstoasc"])
        .arg(&tmp)
        .output()
        .await
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;
    if !status.status.success() {
        std::fs::remove_file(&tmp).ok();
        return Err(error_text(&status.stderr));
    }
    std::fs::remove_file(recording).ok();
    std::fs::rename(&tmp, out).map_err(|e| e.to_string())?;
    Ok(out.to_owned())
}

/// The file yt-dlp wrote for `<stem>.%(ext)s`, found on disk: for when the line naming it got
/// lost. Its partial and helper files are not it.
pub(crate) fn written_file(stem: &Path) -> Option<PathBuf> {
    let dir = stem.parent()?;
    let prefix = format!("{}.", stem.file_name()?.to_string_lossy());
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = name.strip_prefix(&prefix).unwrap_or_default();
            name.starts_with(&prefix)
                && !ext.is_empty()
                && !ext.contains('.')
                && !matches!(ext, "part" | "ytdl" | "temp" | "txt")
        })
        // Video and audio parts are joined into the largest file.
        .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
}
