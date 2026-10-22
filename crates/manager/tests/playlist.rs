//! Playlists and channels through the real yt-dlp and YouTube (network: `--ignored`, with
//! `SHARDLOAD_TOOLS` a folder holding yt-dlp.exe and deno.exe), and groups of downloads.

use std::path::PathBuf;

use shardload_manager::ytdlp::{Tools, inspect};
use shardload_manager::{Config, Manager, NewTask};

fn tools() -> Tools {
    let dir = PathBuf::from(std::env::var("SHARDLOAD_TOOLS").expect("SHARDLOAD_TOOLS"));
    Tools {
        ytdlp: dir.join(if cfg!(windows) {
            "yt-dlp.exe"
        } else {
            "yt-dlp"
        }),
        deno: Some(dir.join(if cfg!(windows) { "deno.exe" } else { "deno" })),
        ffmpeg: None,
    }
}

fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!("shardload-playlist-{}", std::process::id()))
}

#[tokio::test]
#[ignore = "network: YouTube"]
async fn a_youtube_channel_lists_its_newest_videos() {
    let info = inspect(
        &tools(),
        "https://www.youtube.com/@NASA",
        &[],
        &scratch(),
        false,
    )
    .await
    .expect("inspect");
    let list = info.playlist.expect("a channel's videos");
    assert!(list.channel);
    assert_eq!(list.uploader.as_deref(), Some("NASA"));
    assert!(list.entries.len() > 20, "{}", list.entries.len());
    assert!(
        list.entries
            .iter()
            .all(|e| e.url.starts_with("https://www.youtube.com/watch?v=")),
        "only videos, no tabs"
    );
    assert!(list.entries[0].title.is_some());
    assert!(list.entries[0].thumbnail.is_some());
}

#[tokio::test]
#[ignore = "network: YouTube"]
async fn a_youtube_playlist_and_a_video_in_it() {
    let playlist = "https://www.youtube.com/playlist?list=PLB29CbKaE2OY";
    let info = inspect(&tools(), playlist, &[], &scratch(), false)
        .await
        .expect("inspect");
    let list = info.playlist.expect("a playlist");
    assert!(!list.channel);
    assert!(!list.entries.is_empty());
    let first = list.entries[0].url.clone();

    // One of its videos with the list in its address: the video, saying it is in a list.
    let video = format!("{first}&list=PLB29CbKaE2OY");
    let one = inspect(&tools(), &video, &[], &scratch(), false)
        .await
        .expect("inspect");
    assert!(one.playlist.is_none());
    assert!(one.in_playlist);
    assert!(!one.heights.is_empty());
    let whole = inspect(&tools(), &video, &[], &scratch(), true)
        .await
        .expect("inspect");
    assert_eq!(
        whole.playlist.map(|p| p.entries.len()),
        Some(list.entries.len())
    );
}

#[tokio::test]
async fn downloads_added_together_share_a_group() {
    let dir = std::env::temp_dir().join(format!("shardload-groups-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("dir");
    let m = Manager::open(
        &dir.join("list.db"),
        Config {
            dir: dir.join("downloads"),
            ..Config::default()
        },
    )
    .await
    .expect("open");
    let group = m.new_group("  Lectures ").await.expect("group");
    assert_eq!(group.name, "Lectures");
    let mut ids = Vec::new();
    for i in 0..3 {
        let t = m
            .add(NewTask {
                url: format!("http://127.0.0.1:9/{i}.bin"),
                group: Some(group.id),
                ..NewTask::default()
            })
            .await
            .expect("add");
        assert_eq!(t.group_id, Some(group.id));
        ids.push(t.id);
    }
    let alone = m
        .add(NewTask {
            url: "http://127.0.0.1:9/alone.bin".into(),
            ..NewTask::default()
        })
        .await
        .expect("add");
    assert_eq!(alone.group_id, None);
    assert_eq!(m.groups().await.expect("groups"), vec![group.clone()]);
    // An empty group is not listed any more.
    for id in ids {
        m.remove(id, false).await.expect("remove");
    }
    assert!(m.groups().await.expect("groups").is_empty());
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

/// The first program `name` on PATH.
fn on_path(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "network: YouTube; needs ffmpeg and ffprobe on PATH"]
async fn a_clip_of_a_youtube_video() {
    use shardload_manager::clip::Clip;
    use shardload_manager::{Status, Stream};
    let ffmpeg = on_path("ffmpeg").expect("ffmpeg on PATH");
    let dir = std::env::temp_dir().join(format!("shardload-yt-clip-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("dir");
    let t = tools();
    let m = Manager::open(
        &dir.join("list.db"),
        Config {
            dir: dir.join("downloads"),
            ytdlp: Some(t.ytdlp),
            deno: t.deno,
            ffmpeg: Some(ffmpeg),
            ..Config::default()
        },
    )
    .await
    .expect("open");
    let task = m
        .add(NewTask {
            url: "https://www.youtube.com/watch?v=jNQXAC9IVRw".into(),
            name: Some("Me at the zoo.mp4".into()),
            start: true,
            stream: Some(Stream {
                ytdlp: Some(shardload_manager::ytdlp::selector(Some(360))),
                clip: Some(Clip {
                    start: 5.0,
                    end: 12.0,
                    exact: false,
                }),
                ..Stream::default()
            }),
            ..NewTask::default()
        })
        .await
        .expect("add");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let done = loop {
        let t = m.get(task.id).await.expect("get");
        if matches!(t.status, Status::Done | Status::Error) {
            break t;
        }
        assert!(std::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    };
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    let path = done.path.expect("path");
    assert!(
        path.to_string_lossy().contains("Me at the zoo (0.05-0.12)"),
        "{}",
        path.display()
    );
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(&path)
        .output()
        .expect("ffprobe");
    let seconds: f64 = String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse()
        .expect("duration");
    assert!((6.0..=8.0).contains(&seconds), "{seconds} s");
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "network: a YouTube live stream; needs ffmpeg and ffprobe on PATH"]
async fn a_youtube_live_stream_is_recorded_for_its_limit() {
    use shardload_manager::{Recording, Status, Stream};
    let ffmpeg = on_path("ffmpeg").expect("ffmpeg on PATH");
    let dir = std::env::temp_dir().join(format!("shardload-yt-live-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("dir");
    let t = tools();
    let m = Manager::open(
        &dir.join("list.db"),
        Config {
            dir: dir.join("downloads"),
            ytdlp: Some(t.ytdlp),
            deno: t.deno,
            ffmpeg: Some(ffmpeg),
            ..Config::default()
        },
    )
    .await
    .expect("open");
    // Lofi Girl's around-the-clock stream.
    let task = m
        .add(NewTask {
            url: "https://www.youtube.com/@LofiGirl/live".into(),
            name: Some("lofi.mp4".into()),
            start: true,
            stream: Some(Stream {
                ytdlp: Some(shardload_manager::ytdlp::selector(Some(360))),
                record: Some(Recording {
                    limit: Some(10),
                    ..Recording::default()
                }),
                ..Stream::default()
            }),
            ..NewTask::default()
        })
        .await
        .expect("add");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let done = loop {
        let t = m.get(task.id).await.expect("get");
        if matches!(t.status, Status::Done | Status::Error) {
            break t;
        }
        assert!(std::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    };
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    let path = done.path.expect("path");
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=format_name,duration",
            "-of",
            "csv=p=0",
        ])
        .arg(&path)
        .output()
        .expect("ffprobe");
    let report = String::from_utf8_lossy(&probe.stdout).into_owned();
    assert!(report.contains("mp4"), "{report}");
    let seconds: f64 = report
        .trim()
        .rsplit(',')
        .next()
        .and_then(|s| s.parse().ok())
        .expect("duration");
    assert!(seconds >= 3.0, "{seconds} s");
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}
