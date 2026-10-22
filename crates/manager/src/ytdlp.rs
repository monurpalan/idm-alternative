//! yt-dlp, for sites that hide their media behind their own players (YouTube, X, Instagram…):
//! reading what a page offers, and downloading the chosen format with progress.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;
use shardload_engine::{CancellationToken, Phase, Progress};
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
    /// The page is a playlist or a channel: its videos, without their formats (each is read
    /// when its download starts).
    pub playlist: Option<Playlist>,
    /// The page is one video of a playlist (`watch?v=…&list=…`): reading it again with the
    /// whole list gives the playlist.
    pub in_playlist: bool,
}

/// A playlist or a channel's videos (a channel's newest first).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Playlist {
    pub title: Option<String>,
    /// Who made it: the channel's or the playlist owner's name.
    pub uploader: Option<String>,
    /// A channel's videos rather than a playlist.
    pub channel: bool,
    pub entries: Vec<PlaylistEntry>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlaylistEntry {
    /// The video's own page.
    pub url: String,
    pub title: Option<String>,
    /// Seconds.
    pub duration: Option<f64>,
    /// A small picture of it, when the site gives one.
    pub thumbnail: Option<String>,
}

/// At most this many videos are listed from a playlist or a channel.
pub const MAX_ENTRIES: usize = 1000;

/// The yt-dlp format selector for a choice: a height (best video up to it with the best
/// audio), or audio alone.
pub fn selector(height: Option<u32>) -> String {
    match height {
        Some(h) => format!("bestvideo[height<={h}]+bestaudio/best[height<={h}]/best"),
        None => "bestaudio/best".to_owned(),
    }
}

/// Whether a format selector asks yt-dlp to merge two formats (`video+audio`), which needs
/// ffmpeg.
pub fn merges(format: &str) -> bool {
    format.split('/').any(|choice| choice.contains('+'))
}

/// The selector without its merging choices, for when ffmpeg is missing: one format that
/// has both video and audio, so yt-dlp never leaves a silent video next to its sound.
pub fn single_file(format: &str) -> String {
    let single: Vec<&str> = format
        .split('/')
        .filter(|choice| !choice.contains('+'))
        .collect();
    if single.is_empty() {
        "best".to_owned()
    } else {
        single.join("/")
    }
}

/// How formats are ranked: the chosen height and frame rate first, then H.264 video and AAC
/// audio, which every player opens. yt-dlp's own default puts AV1 and Opus first, which
/// Windows' player cannot open without an extension. Its `-t mp4` preset ranks the codec
/// before the resolution and so never goes past 1080p on YouTube; here a height only H.264
/// lacks (1440p, 4K) still comes as VP9 or AV1.
const FORMAT_SORT: &str = "res,fps,vcodec:h264,acodec:aac";

/// A title without the `...` yt-dlp cuts long posts with (X: `user - start of the post...`),
/// and without the cut word before it. Only when the title is a cut of the description, so a
/// video named "Wait for it..." keeps its dots.
fn tidy_title(title: &str, description: Option<&str>) -> String {
    let title = title.trim();
    let Some(cut) = title
        .strip_suffix("...")
        .or_else(|| title.strip_suffix('…'))
    else {
        return title.to_owned();
    };
    let body = cut.split_once(" - ").map_or(cut, |(_, b)| b).trim();
    // What the description goes on with after the cut.
    let Some(rest) = description
        .map(str::trim)
        .and_then(|d| d.strip_prefix(body))
        .filter(|r| !body.is_empty() && !r.is_empty())
    else {
        return title.to_owned();
    };
    let word_cut = rest.starts_with(char::is_alphanumeric);
    let kept = match cut.trim_end().rfind(char::is_whitespace) {
        Some(i) if word_cut => &cut[..i],
        _ => cut,
    };
    let kept = kept.trim_end_matches(|c: char| c.is_whitespace() || ",;:-–—".contains(c));
    if kept.is_empty() {
        title.to_owned()
    } else {
        kept.to_owned()
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

/// The arguments common to reading and downloading: tools, cookies and headers. A page that
/// is both a video and a playlist is taken as the video, unless `whole_list`.
fn base_args(
    tools: &Tools,
    url: &str,
    headers: &[(String, String)],
    cookies: Option<&Path>,
) -> Vec<String> {
    list_args(tools, url, headers, cookies, false)
}

fn list_args(
    tools: &Tools,
    url: &str,
    headers: &[(String, String)],
    cookies: Option<&Path>,
    whole_list: bool,
) -> Vec<String> {
    let mut args = vec![
        "--encoding".to_owned(),
        "utf-8".to_owned(),
        "--no-warnings".to_owned(),
        "--ignore-config".to_owned(),
    ];
    if !whole_list {
        args.push("--no-playlist".into());
    }
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
    // UTF-8 output whatever the console code page: titles and paths in other scripts otherwise
    // reach us in the system's legacy encoding (seen with Dailymotion, TikTok, Bilibili).
    cmd.env("PYTHONIOENCODING", "utf-8").env("PYTHONUTF8", "1");
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

/// A YouTube channel's own page (`/@name`, `/channel/…`, `/c/…`, `/user/…`) lists its tabs;
/// its videos are on `/videos`, newest first.
fn channel_videos(url: &str) -> Option<String> {
    let mut u = url::Url::parse(url).ok()?;
    let host = u
        .host_str()?
        .trim_start_matches("www.")
        .trim_start_matches("m.");
    if host != "youtube.com" {
        return None;
    }
    let parts: Vec<String> = u
        .path_segments()?
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let root = match parts.as_slice() {
        [handle] => handle.starts_with('@'),
        [kind, _] => matches!(kind.as_str(), "channel" | "c" | "user"),
        _ => false,
    };
    if !root {
        return None;
    }
    u.set_path(&format!("/{}/videos", parts.join("/")));
    Some(u.to_string())
}

/// What `url` offers, read with `yt-dlp -J`: a video's qualities, or a playlist's or a
/// channel's videos (listed flat, without reading each). With `whole_list`, a video of a
/// playlist is read as its playlist. `scratch` is a folder for the cookie file.
pub async fn inspect(
    tools: &Tools,
    url: &str,
    headers: &[(String, String)],
    scratch: &Path,
    whole_list: bool,
) -> Result<PageInfo, String> {
    std::fs::create_dir_all(scratch).ok();
    let channel = channel_videos(url);
    let url = channel.as_deref().unwrap_or(url);
    let cookie_path = scratch.join(format!("cookies-{}.txt", std::process::id()));
    let cookies = write_cookies(&cookie_path, url, headers);
    let mut args = vec![
        "-J".to_owned(),
        "--flat-playlist".to_owned(),
        "-I".to_owned(),
        format!(":{MAX_ENTRIES}"),
    ];
    args.extend(list_args(
        tools,
        url,
        headers,
        cookies.as_deref(),
        whole_list,
    ));
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
    let mut info = page_info(&v);
    if let Some(p) = info.playlist.as_mut() {
        p.channel |= channel.is_some();
    }
    info.in_playlist = info.playlist.is_none() && !whole_list && in_playlist(url);
    Ok(info)
}

/// A video page that names a playlist too (`watch?v=…&list=…`).
fn in_playlist(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| {
        let keys: Vec<String> = u.query_pairs().map(|(k, _)| k.into_owned()).collect();
        keys.iter().any(|k| k == "list") && keys.iter().any(|k| k == "v")
    })
}

/// A playlist's videos, from yt-dlp's flat listing.
fn playlist(v: &serde_json::Value) -> Option<Playlist> {
    if v["_type"].as_str() != Some("playlist") {
        return None;
    }
    let entries = v["entries"]
        .as_array()
        .map(|e| e.iter().filter_map(entry).collect())
        .unwrap_or_default();
    let uploader = v["uploader"]
        .as_str()
        .or_else(|| v["channel"].as_str())
        .map(str::to_owned);
    Some(Playlist {
        title: v["title"].as_str().map(str::to_owned),
        uploader,
        // A channel's list is the channel itself.
        channel: v["channel_id"].as_str().is_some() && v["channel_id"] == v["id"],
        entries,
    })
}

fn entry(e: &serde_json::Value) -> Option<PlaylistEntry> {
    // A channel's own page lists its tabs as playlists: not videos.
    if e["_type"].as_str() == Some("playlist") {
        return None;
    }
    let url = e["url"]
        .as_str()
        .or_else(|| e["webpage_url"].as_str())
        .filter(|u| u.starts_with("http"))
        .map(str::to_owned)
        .or_else(|| {
            // YouTube's flat entries may give only the id.
            let id = e["id"].as_str()?;
            (e["ie_key"].as_str() == Some("Youtube"))
                .then(|| format!("https://www.youtube.com/watch?v={id}"))
        })?;
    let thumbnail = e["thumbnails"]
        .as_array()
        .and_then(|t| {
            // The smallest that is not tiny: the list is for choosing, not watching.
            t.iter()
                .filter(|t| t["width"].as_u64().is_none_or(|w| w >= 120))
                .min_by_key(|t| t["width"].as_u64().unwrap_or(u64::MAX))
                .or_else(|| t.last())
        })
        .and_then(|t| t["url"].as_str())
        .or_else(|| e["thumbnail"].as_str())
        .map(str::to_owned);
    Some(PlaylistEntry {
        url,
        title: e["title"].as_str().map(str::to_owned),
        duration: e["duration"].as_f64(),
        thumbnail,
    })
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
    let playlist = playlist(v);
    PageInfo {
        title: v["title"]
            .as_str()
            .map(|t| tidy_title(t, v["description"].as_str())),
        duration: v["duration"].as_f64(),
        // A list's videos are read one by one as they download: any height may come.
        heights: if playlist.is_some() {
            LIST_HEIGHTS.to_vec()
        } else {
            heights
        },
        audio_only: audio_only || playlist.is_some(),
        playlist,
        in_playlist: false,
    }
}

/// Heights offered for a whole list; each video gets the best it has up to the one chosen.
const LIST_HEIGHTS: [u32; 6] = [2160, 1440, 1080, 720, 480, 360];

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
        .or_else(|| written_file(output_stem))
        .ok_or_else(|| "yt-dlp finished without saying which file it wrote".to_owned())
}

/// The file yt-dlp wrote for `<stem>.%(ext)s`, found on disk: for when the line naming it got
/// lost. Its partial and helper files are not it.
fn written_file(stem: &Path) -> Option<PathBuf> {
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
    fn selectors_without_ffmpeg() {
        let video = selector(Some(1080));
        assert!(merges(&video));
        assert_eq!(single_file(&video), "best[height<=1080]/best");
        assert!(!merges(&single_file(&video)));
        // Audio alone is one format already.
        assert!(!merges(&selector(None)));
        assert_eq!(single_file(&selector(None)), "bestaudio/best");
        assert_eq!(single_file("bv*+ba"), "best");
    }

    #[test]
    fn cut_titles() {
        let post = Some("The Orion spacecraft separated from the upper stage of the rocket.");
        assert_eq!(
            tidy_title(
                "NASA - The Orion spacecraft separated from the upper stage of t...",
                post
            ),
            "NASA - The Orion spacecraft separated from the upper stage of"
        );
        let post = Some("Bir gün, ne güzel bir gün");
        assert_eq!(
            tidy_title("Ali - Bir gün, ne güzel …", post),
            "Ali - Bir gün, ne güzel"
        );
        assert_eq!(tidy_title("Ali - Bir gün, ...", post), "Ali - Bir gün");
        // The last word is whole in the description: kept.
        assert_eq!(
            tidy_title("Ali - Bir gün, ne...", post),
            "Ali - Bir gün, ne"
        );
        assert_eq!(tidy_title("Ali - Bir...", Some("Birdenbire")), "Ali");
        // Not a cut of the description: kept as it is.
        assert_eq!(
            tidy_title("Wait for it...", Some("A cat")),
            "Wait for it..."
        );
        assert_eq!(tidy_title("Wait for it...", None), "Wait for it...");
        assert_eq!(tidy_title("Me at the zoo", post), "Me at the zoo");
        assert_eq!(tidy_title("...", post), "...");
    }

    #[test]
    fn finds_the_written_file() {
        let dir = std::env::temp_dir().join(format!("shardload-ytdlp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let stem = dir.join("Клип 視頻");
        for (name, len) in [
            ("Клип 視頻.f137.mp4.part", 9),
            ("Клип 視頻.mp4.ytdl", 1),
            ("Клип 視頻.shardload-cookies.txt", 1),
            ("Клип 視頻.mp4", 5),
            ("other.mp4", 50),
        ] {
            std::fs::write(dir.join(name), vec![0u8; len]).expect("write");
        }
        assert_eq!(written_file(&stem), Some(dir.join("Клип 視頻.mp4")));
        std::fs::remove_dir_all(&dir).ok();
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
            parse_progress("shardload 1024 4096 NA 512.5"),
            Some((1024, Some(4096), 512.5))
        );
        assert_eq!(
            parse_progress("shardload 10 NA 2000.0 NA"),
            Some((10, Some(2000), 0.0))
        );
        assert_eq!(parse_progress("[download] 5%"), None);
        assert_eq!(parse_progress("shardload NA NA NA NA"), None);
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
                audio_only: true,
                playlist: None,
                in_playlist: false,
            }
        );
    }

    /// Trimmed from `yt-dlp -J --flat-playlist` on a YouTube playlist (2026-10).
    const PLAYLIST: &str = r#"{
        "_type": "playlist", "id": "PLx", "title": "Lectures", "uploader": "MIT",
        "channel_id": "UCabc", "playlist_count": 3,
        "entries": [
            {"_type": "url", "ie_key": "Youtube", "id": "a1", "url": "https://www.youtube.com/watch?v=a1",
             "title": "Lecture 1", "duration": 3021.0,
             "thumbnails": [{"url": "https://i.ytimg.com/vi/a1/hq1.jpg", "width": 168},
                            {"url": "https://i.ytimg.com/vi/a1/hq2.jpg", "width": 336},
                            {"url": "https://i.ytimg.com/vi/a1/tiny.jpg", "width": 64}]},
            {"_type": "url", "ie_key": "Youtube", "id": "a2", "title": "[Private video]",
             "duration": null, "thumbnails": []},
            {"_type": "url", "ie_key": "Generic", "id": "x", "title": "No address"}
        ]
    }"#;

    /// A channel's videos tab, as listed: the channel is its own uploader.
    const CHANNEL: &str = r#"{
        "_type": "playlist", "id": "UCabc", "title": "MIT - Videos", "channel": "MIT",
        "channel_id": "UCabc",
        "entries": [
            {"_type": "url", "ie_key": "Youtube", "id": "n1", "url": "https://www.youtube.com/watch?v=n1",
             "title": "Newest", "duration": 61, "thumbnail": "https://i.ytimg.com/vi/n1/default.jpg"},
            {"_type": "playlist", "id": "UCabc", "title": "MIT - Shorts", "url": "https://www.youtube.com/@mit/shorts"}
        ]
    }"#;

    #[test]
    fn playlists_from_json() {
        let v: serde_json::Value = serde_json::from_str(PLAYLIST).expect("json");
        let info = page_info(&v);
        let p = info.playlist.expect("a playlist");
        assert_eq!(p.title.as_deref(), Some("Lectures"));
        assert_eq!(p.uploader.as_deref(), Some("MIT"));
        assert!(!p.channel);
        assert_eq!(p.entries.len(), 2, "the one without an address is left out");
        assert_eq!(p.entries[0].url, "https://www.youtube.com/watch?v=a1");
        assert_eq!(p.entries[0].duration, Some(3021.0));
        assert_eq!(
            p.entries[0].thumbnail.as_deref(),
            Some("https://i.ytimg.com/vi/a1/hq1.jpg")
        );
        assert_eq!(p.entries[1].url, "https://www.youtube.com/watch?v=a2");
        assert_eq!(p.entries[1].thumbnail, None);
        assert!(info.audio_only);
        assert_eq!(info.heights[0], 2160);

        let v: serde_json::Value = serde_json::from_str(CHANNEL).expect("json");
        let p = page_info(&v).playlist.expect("a channel");
        assert!(p.channel);
        assert_eq!(p.uploader.as_deref(), Some("MIT"));
        assert_eq!(p.entries.len(), 1, "tabs are not videos");
        assert_eq!(
            p.entries[0].thumbnail.as_deref(),
            Some("https://i.ytimg.com/vi/n1/default.jpg")
        );
    }

    #[test]
    fn channel_and_playlist_addresses() {
        for (url, want) in [
            (
                "https://www.youtube.com/@mit",
                Some("https://www.youtube.com/@mit/videos"),
            ),
            (
                "https://youtube.com/channel/UCabc",
                Some("https://youtube.com/channel/UCabc/videos"),
            ),
            ("https://www.youtube.com/@mit/shorts", None),
            ("https://www.youtube.com/watch?v=a1", None),
            ("https://vimeo.com/@someone", None),
        ] {
            assert_eq!(channel_videos(url).as_deref(), want, "{url}");
        }
        assert!(in_playlist("https://www.youtube.com/watch?v=a1&list=PLx"));
        assert!(!in_playlist("https://www.youtube.com/playlist?list=PLx"));
        assert!(!in_playlist("https://www.youtube.com/watch?v=a1"));
    }
}
