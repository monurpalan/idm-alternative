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
