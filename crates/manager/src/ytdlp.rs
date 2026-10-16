//! yt-dlp, for sites that hide their media behind their own players (YouTube, X, Instagram…):
//! reading what a page offers, and downloading the chosen format with progress.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use idma_engine::{CancellationToken, Phase, Progress};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::watch;

/// Programs yt-dlp runs with.
#[derive(Debug, Clone)]
pub struct Tools {
    pub ytdlp: PathBuf,
    /// JavaScript runtime yt-dlp needs for YouTube.
    pub deno: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
}

/// What a page offers, simplified for choosing.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PageInfo {
    pub title: Option<String>,
    /// Seconds.
    pub duration: Option<f64>,
    /// Video heights available, highest first.
    pub heights: Vec<u32>,
    /// A format with audio alone exists.
    pub audio_only: bool,
}

/// The yt-dlp format selector for a choice: a height (best video up to it with the best
/// audio), or audio alone.
pub fn selector(height: Option<u32>) -> String {
    match height {
        Some(h) => format!("bestvideo[height<={h}]+bestaudio/best[height<={h}]/best"),
        None => "bestaudio/best".to_owned(),
    }
}

/// Netscape cookie file lines for a `Cookie` header sent to `url`.
pub(crate) fn cookie_file(cookie_header: &str, url: &str) -> Option<String> {
    let host = url::Url::parse(url).ok()?.host_str()?.to_owned();
    let mut out = String::from("# Netscape HTTP Cookie File\n");
    for pair in cookie_header.split(';') {
        let Some((name, value)) = pair.trim().split_once('=') else {
            continue;
        };
        // domain, include subdomains, path, secure, expiry (0: session), name, value
        out.push_str(&format!(".{host}\tTRUE\t/\tFALSE\t0\t{name}\t{value}\n"));
    }
    Some(out)
}

/// The arguments common to reading and downloading: tools, cookies and headers.
fn base_args(
    tools: &Tools,
    url: &str,
    headers: &[(String, String)],
    cookies: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        "--no-playlist".to_owned(),
        "--no-warnings".to_owned(),
        "--ignore-config".to_owned(),
    ];
    if let Some(deno) = &tools.deno {
        args.push("--js-runtimes".into());
        args.push(format!("deno:{}", deno.display()));
    }
    if let Some(ffmpeg) = &tools.ffmpeg {
        args.push("--ffmpeg-location".into());
        args.push(ffmpeg.display().to_string());
    }
    if let Some(c) = cookies {
        args.push("--cookies".into());
        args.push(c.display().to_string());
    }
    for (k, v) in headers {
        match k.to_ascii_lowercase().as_str() {
            "cookie" => {}
            "user-agent" => {
                args.push("--user-agent".into());
                args.push(v.clone());
            }
            "referer" => {
                args.push("--referer".into());
                args.push(v.clone());
            }
            _ => {
                args.push("--add-headers".into());
                args.push(format!("{k}:{v}"));
            }
        }
    }
    args.push(url.to_owned());
    args
}

/// Writes the cookie file for `headers` (if they carry cookies) at `path`.
fn write_cookies(path: &Path, url: &str, headers: &[(String, String)]) -> Option<PathBuf> {
    let cookie = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("cookie"))
        .map(|(_, v)| v.as_str())?;
    std::fs::write(path, cookie_file(cookie, url)?).ok()?;
    Some(path.to_owned())
}

fn command(exe: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(exe);
    cmd.stdin(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    {
        // No console window flashing up from a GUI app.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// The last line of yt-dlp's errors, without its prefix.
fn error_text(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("yt-dlp failed")
        .trim()
        .trim_start_matches("ERROR: ")
        .to_owned()
}

/// What `url` offers, read with `yt-dlp -J`. `scratch` is a folder for the cookie file.
pub async fn inspect(
    tools: &Tools,
    url: &str,
    headers: &[(String, String)],
    scratch: &Path,
) -> Result<PageInfo, String> {
    std::fs::create_dir_all(scratch).ok();
    let cookie_path = scratch.join(format!("cookies-{}.txt", std::process::id()));
    let cookies = write_cookies(&cookie_path, url, headers);
    let mut args = vec!["-J".to_owned()];
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
    Ok(page_info(&v))
}

fn page_info(v: &serde_json::Value) -> PageInfo {
    let formats = v["formats"].as_array().cloned().unwrap_or_default();
    let mut heights: Vec<u32> = formats
        .iter()
        .filter(|f| f["vcodec"].as_str().is_some_and(|c| c != "none"))
        .filter_map(|f| f["height"].as_u64().map(|h| h as u32))
        .collect();
    heights.sort_unstable_by(|a, b| b.cmp(a));
    heights.dedup();
    let audio_only = formats.iter().any(|f| {
        f["vcodec"].as_str() == Some("none") && f["acodec"].as_str().is_some_and(|c| c != "none")
    });
    PageInfo {
        title: v["title"].as_str().map(str::to_owned),
        duration: v["duration"].as_f64(),
        heights,
        audio_only,
    }
}

/// One progress line from our `--progress-template`: downloaded, total, estimated total,
/// speed (`NA` when unknown).
pub(crate) fn parse_progress(line: &str) -> Option<(u64, Option<u64>, f64)> {
    let rest = line.strip_prefix("idma ")?;
    let mut it = rest.split_whitespace();
    let num = |s: Option<&str>| s.and_then(|s| s.parse::<f64>().ok());
    let downloaded = num(it.next())? as u64;
    let total = num(it.next()).or(num(it.next())).map(|t| t as u64);
    let speed = num(it.next()).unwrap_or(0.0);
    Some((downloaded, total, speed))
}

/// Downloads `url` in `format` to `<dir>/<stem>.<ext>` and returns the file written. yt-dlp
/// downloads video and audio one after the other and joins them; progress covers both.
/// Cancelling stops yt-dlp; running again continues its partial files.
pub async fn run(
    tools: &Tools,
    url: &str,
    format: &str,
    output_stem: &Path,
    headers: &[(String, String)],
    progress: &watch::Sender<Progress>,
    cancel: &CancellationToken,
) -> Result<PathBuf, String> {
    let cookie_path = PathBuf::from(format!("{}.idma-cookies.txt", output_stem.display()));
    let cookies = write_cookies(&cookie_path, url, headers);
    let mut args = vec![
        "-f".to_owned(),
        format.to_owned(),
        "--merge-output-format".into(),
        "mp4".into(),
        "--continue".into(),
        "--newline".into(),
        "--progress-template".into(),
        "download:idma %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s %(progress.speed)s".into(),
        "--print".into(),
        "after_move:idma-file %(filepath)s".into(),
        "-o".into(),
        format!("{}.%(ext)s", output_stem.display()),
    ];
    args.extend(base_args(tools, url, headers, cookies.as_deref()));
    let mut child = command(&tools.ytdlp)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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

    let mut lines = BufReader::new(stdout).lines();
    let mut file: Option<PathBuf> = None;
    // Bytes of the files already finished (video before audio), and the last count.
    let mut finished = 0u64;
    let mut last = (0u64, None::<u64>);
    progress.send_modify(|p| p.phase = Phase::Downloading);
    let result = loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if let Some(path) = line.strip_prefix("idma-file ") {
                        file = Some(PathBuf::from(path.trim()));
                    } else if let Some((got, total, speed)) = parse_progress(&line) {
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
                child.kill().await.ok();
                if let Some(c) = &cookies {
                    std::fs::remove_file(c).ok();
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
        .ok_or_else(|| "yt-dlp finished without saying which file it wrote".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors() {
        assert_eq!(
            selector(Some(720)),
            "bestvideo[height<=720]+bestaudio/best[height<=720]/best"
        );
        assert_eq!(selector(None), "bestaudio/best");
    }

    #[test]
    fn cookie_files() {
        let f = cookie_file(
            "SID=abc; pref=a=b; junk",
            "https://www.youtube.com/watch?v=1",
        )
        .expect("file");
        let lines: Vec<&str> = f.lines().collect();
        assert_eq!(lines[0], "# Netscape HTTP Cookie File");
        assert_eq!(lines[1], ".www.youtube.com\tTRUE\t/\tFALSE\t0\tSID\tabc");
        assert_eq!(lines[2], ".www.youtube.com\tTRUE\t/\tFALSE\t0\tpref\ta=b");
        assert_eq!(lines.len(), 3, "a pair without = is skipped");
        assert!(cookie_file("a=1", "not a url").is_none());
    }

    #[test]
    fn progress_lines() {
        assert_eq!(
            parse_progress("idma 1024 4096 NA 512.5"),
            Some((1024, Some(4096), 512.5))
        );
        assert_eq!(
            parse_progress("idma 10 NA 2000.0 NA"),
            Some((10, Some(2000), 0.0))
        );
        assert_eq!(parse_progress("[download] 5%"), None);
        assert_eq!(parse_progress("idma NA NA NA NA"), None);
    }

    #[test]
    fn page_info_from_json() {
        let v = serde_json::json!({
            "title": "Klip",
            "duration": 12.5,
            "formats": [
                {"format_id": "a", "vcodec": "none", "acodec": "mp4a.40.2"},
                {"format_id": "1", "vcodec": "avc1", "acodec": "none", "height": 360},
                {"format_id": "2", "vcodec": "avc1", "acodec": "none", "height": 1080},
                {"format_id": "3", "vcodec": "vp9", "acodec": "none", "height": 1080},
                {"format_id": "sb", "vcodec": "none", "acodec": "none", "height": 45}
            ]
        });
        assert_eq!(
            page_info(&v),
            PageInfo {
                title: Some("Klip".into()),
                duration: Some(12.5),
                heights: vec![1080, 360],
                audio_only: true
            }
        );
    }
}
