//! A video page's formats fetched by the engine: yt-dlp (a stub here) only says where they
//! are; video and audio come at the same time over several connections, in requests no
//! larger than the site serves at full speed, and ffmpeg (a stub too) joins them. What the
//! engine cannot fetch goes to yt-dlp's own download.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use shardload_manager::{Config, Manager, NewTask, Status, Stream, Task};
use shardload_testserver::{Verdict, verify_file};

const VIDEO: u64 = 3 * MB;
const AUDIO: u64 = 512 << 10;

/// Writes `name` as an executable script: `windows` for cmd, `unix` for sh.
fn script(dir: &Path, name: &str, windows: &str, unix: &str) -> PathBuf {
    if cfg!(windows) {
        let path = dir.join(format!("{name}.cmd"));
        std::fs::write(&path, windows.replace('\n', "\r\n")).expect("write");
        path
    } else {
        let path = dir.join(name);
        std::fs::write(&path, unix).expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        path
    }
}

/// A yt-dlp that answers `-J` with `info` and otherwise "downloads" by copying `fallback`
/// to `<dir>/downloads/video.bin`, naming it as ours does.
fn fake_ytdlp(dir: &Path, info: &serde_json::Value, fallback: &[u8]) -> PathBuf {
    std::fs::write(dir.join("info.json"), info.to_string()).expect("info");
    std::fs::write(dir.join("fallback.bin"), fallback).expect("fallback");
    let out = dir.join("downloads").join("video.bin");
    let windows = format!(
        "@echo off\nif \"%~1\"==\"-J\" (\n  type \"%~dp0info.json\"\n  exit /b 0\n)\n\
         copy /y /b \"%~dp0fallback.bin\" \"{out}\" >nul\necho shardload-file {out}\n",
        out = out.display()
    );
    let unix = format!(
        "#!/bin/sh\nif [ \"$1\" = \"-J\" ]; then cat \"$(dirname \"$0\")/info.json\"; exit 0; fi\n\
         cp \"$(dirname \"$0\")/fallback.bin\" '{out}'\necho 'shardload-file {out}'\n",
        out = out.display()
    );
    script(dir, "yt-dlp", &windows, &unix)
}

/// An ffmpeg that joins its two inputs by putting one after the other: `-i` 5th and 7th
/// argument, the output last.
fn fake_ffmpeg(dir: &Path) -> PathBuf {
    let windows = "@echo off\nset V=%~6\nset A=%~8\n:loop\nif \"%~1\"==\"\" goto done\n\
                   set last=%~1\nshift\ngoto loop\n:done\ncopy /y /b \"%V%\"+\"%A%\" \"%last%\" >nul\n";
    let unix =
        "#!/bin/sh\nv=\"$6\"; a=\"$8\"; for last; do :; done\ncat \"$v\" \"$a\" > \"$last\"\n";
    script(dir, "ffmpeg", windows, unix)
}

fn format(base: &str, id: &str, file: &str, size: u64, seed: u64) -> serde_json::Value {
    serde_json::json!({
        "format_id": id,
        "protocol": "https",
        "ext": file.rsplit('.').next(),
        "filesize": size,
        "url": format!("{base}/file/{file}?size={size}&seed={seed}&only_ua=FakeTube"),
        "http_headers": {"User-Agent": "Mozilla/5.0 FakeTube"},
        "downloader_options": {"http_chunk_size": MB},
    })
}

async fn manager(w: &WorkDir, ytdlp: PathBuf, ffmpeg: Option<PathBuf>) -> Manager {
    Manager::open(
        &w.db(),
        Config {
            ytdlp: Some(ytdlp),
            ffmpeg,
            connections: 6,
            ..w.config(1)
        },
    )
    .await
    .expect("open")
}

async fn add(m: &Manager) -> Task {
    m.add(NewTask {
        url: "https://video.example/watch?v=x".into(),
        name: Some("video.mp4".into()),
        start: true,
        stream: Some(Stream {
            ytdlp: Some(shardload_manager::ytdlp::selector(Some(1080))),
            ..Stream::default()
        }),
        ..NewTask::default()
    })
    .await
    .expect("add")
}

/// `range` of the file at `path` checked against the test server's bytes for `seed`.
fn assert_slice(path: &Path, range: std::ops::Range<usize>, seed: u64, dir: &Path) {
    let data = std::fs::read(path).expect("read");
    let piece = dir.join(format!("piece-{seed}"));
    std::fs::write(&piece, &data[range.clone()]).expect("write");
    assert_eq!(
        verify_file(&piece, seed, range.len() as u64).expect("verify"),
        Verdict::Ok,
        "bytes {range:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn video_and_audio_come_together_in_small_requests_and_are_joined() {
    let base = start_server().await;
    let w = WorkDir::new("ytdlp-direct");
    let info = serde_json::json!({
        "requested_formats": [
            format(&base, "137", "v.mp4", VIDEO, 11),
            format(&base, "140", "a.m4a", AUDIO, 12),
        ],
    });
    let ytdlp = fake_ytdlp(w.path(), &info, b"not this");
    let m = manager(&w, ytdlp, Some(fake_ffmpeg(w.path()))).await;
    let task = add(&m).await;
    let done = wait_done(&m, task.id).await;

    let path = done.path.expect("path");
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("mp4"));
    assert_eq!(std::fs::metadata(&path).expect("file").len(), VIDEO + AUDIO);
    assert_slice(&path, 0..VIDEO as usize, 11, w.path());
    assert_slice(
        &path,
        VIDEO as usize..(VIDEO + AUDIO) as usize,
        12,
        w.path(),
    );
    // Nothing left of the parts.
    let left: Vec<String> = std::fs::read_dir(path.parent().expect("dir"))
        .expect("list")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["video.mp4"]);
    // Every request asked for one chunk at most, and the video over several connections.
    let ranges = ranges_asked(&base).await;
    assert!(ranges.len() > 4, "{ranges:?}");
    for r in &ranges {
        let (start, end) = r
            .strip_prefix("bytes=")
            .and_then(|r| r.split_once('-'))
            .expect("a range");
        if end.is_empty() {
            continue; // the first request, which learns the size and is cut short
        }
        let len = end.parse::<u64>().expect("end") - start.parse::<u64>().expect("start") + 1;
        assert!(len <= MB, "{r}");
    }
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_format_in_fragments_is_left_to_yt_dlp() {
    let base = start_server().await;
    let w = WorkDir::new("ytdlp-direct-fallback");
    let mut hls = format(&base, "95", "v.mp4", VIDEO, 11);
    hls["protocol"] = "m3u8_native".into();
    let info =
        serde_json::json!({"requested_formats": [hls, format(&base, "140", "a.m4a", AUDIO, 12)]});
    let ytdlp = fake_ytdlp(w.path(), &info, b"from yt-dlp");
    let m = manager(&w, ytdlp, Some(fake_ffmpeg(w.path()))).await;
    let task = add(&m).await;
    let done = wait_done(&m, task.id).await;
    let path = done.path.expect("path");
    assert_eq!(std::fs::read(&path).expect("file"), b"from yt-dlp");
    assert!(
        ranges_asked(&base).await.is_empty(),
        "the engine fetched nothing"
    );
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_download_goes_on_from_where_it_was() {
    let base = start_server().await;
    let w = WorkDir::new("ytdlp-direct-pause");
    let mut video = format(&base, "137", "v.mp4", 8 * MB, 21);
    video["url"] = format!(
        "{base}/file/v.mp4?size={}&seed=21&rate=1M&only_ua=FakeTube",
        8 * MB
    )
    .into();
    let mut audio = format(&base, "140", "a.m4a", 2 * MB, 22);
    audio["url"] = format!(
        "{base}/file/a.m4a?size={}&seed=22&rate=512K&only_ua=FakeTube",
        2 * MB
    )
    .into();
    let info = serde_json::json!({"requested_formats": [video, audio]});
    let ytdlp = fake_ytdlp(w.path(), &info, b"not this");
    let m = manager(&w, ytdlp, Some(fake_ffmpeg(w.path()))).await;
    let task = add(&m).await;
    wait_for(&m, task.id, 30, |t| t.received > 2 * MB).await;
    m.pause(task.id).await.expect("pause");
    let paused = wait_for(&m, task.id, 30, |t| t.status == Status::Paused).await;
    assert!(paused.received < 8 * MB);
    let sent = bytes_sent(&base).await;
    m.start(task.id).await.expect("start");
    let done = wait_done(&m, task.id).await;
    let path = done.path.expect("path");
    assert_slice(&path, 0..(8 * MB) as usize, 21, w.path());
    assert_slice(&path, (8 * MB) as usize..(10 * MB) as usize, 22, w.path());
    // Neither part downloaded again from its start: what was there at the pause stays.
    let again = bytes_sent(&base).await - sent;
    let left = 10 * MB - paused.received;
    assert!(
        again < left + MB,
        "{again} bytes after resuming, {left} were left"
    );
    m.close().await;
}
