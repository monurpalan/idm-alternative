//! Recording live streams through the manager, against the test server's live HLS.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use shardload_manager::{Config, Manager, NewTask, Recording, Status, Stream, Task, TaskId};
use shardload_testserver::hls::track_seed;
use shardload_testserver::{Verdict, router, verify_file};

async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, router()).await });
    format!("http://{addr}")
}

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir()
            .join("shardload-live-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create workdir");
        Self(dir)
    }

    async fn manager(&self) -> Manager {
        Manager::open(
            &self.0.join("list.db"),
            Config {
                dir: self.0.join("downloads"),
                ..Config::default()
            },
        )
        .await
        .expect("open")
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

async fn wait_for(m: &Manager, id: TaskId, secs: u64, pred: impl Fn(&Task) -> bool) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let t = m.get(id).await.expect("get");
        if pred(&t) {
            return t;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out: {:?} {:?}",
            t.status,
            t.error
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn recording(url: String, record: Recording) -> NewTask {
    NewTask {
        url,
        name: Some("Yayın".into()),
        start: true,
        stream: Some(Stream {
            record: Some(record),
            ..Stream::default()
        }),
        ..NewTask::default()
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_stream_is_recorded_until_it_ends() {
    let base = start_server().await;
    let w = WorkDir::new("ends");
    let m = w.manager().await;
    let url =
        format!("{base}/hls/v720.m3u8?live=1&seg_ms=200&window=8&end_after=6&seed=41&seg_size=32K");
    let t = m
        .add(recording(url, Recording::default()))
        .await
        .expect("add");
    let done = wait_for(&m, t.id, 30, |t| {
        t.status != Status::Downloading && t.status != Status::Queued
    })
    .await;
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    // No ffmpeg in this configuration: the joined segments.
    let path = done.path.expect("path");
    assert!(
        path.to_string_lossy().ends_with("Yayın.ts"),
        "{}",
        path.display()
    );
    assert_eq!(
        verify_file(&path, track_seed(41, "v720"), 6 * (32 << 10)).expect("verify"),
        Verdict::Ok
    );
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_recording_finishes_it() {
    let base = start_server().await;
    let w = WorkDir::new("stop");
    let m = w.manager().await;
    let url = format!("{base}/hls/v720.m3u8?live=1&seg_ms=200&window=4&seed=42&seg_size=16K");
    let t = m
        .add(recording(url, Recording::default()))
        .await
        .expect("add");
    wait_for(&m, t.id, 10, |t| t.received > 0).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    // Pausing a recording stops it: the stream does not wait, so it cannot go on later.
    m.pause(t.id).await.expect("stop");
    let done = m.get(t.id).await.expect("get");
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    let size = std::fs::metadata(done.path.expect("path"))
        .expect("file")
        .len();
    assert!(size >= 2 * (16 << 10), "{size}");
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recording_waits_for_its_start_time_and_keeps_to_its_limit() {
    let base = start_server().await;
    let w = WorkDir::new("later");
    let m = w.manager().await;
    let url = format!("{base}/hls/v720.m3u8?live=1&seg_ms=200&window=10&seed=43&seg_size=8K");
    let at = now_ms() + 1500;
    let t = m
        .add(recording(
            url,
            Recording {
                limit: Some(1),
                start_at: Some(at),
                ..Recording::default()
            },
        ))
        .await
        .expect("add");
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(m.get(t.id).await.expect("get").status, Status::Queued);
    let done = wait_for(&m, t.id, 30, |t| {
        t.status == Status::Done || t.status == Status::Error
    })
    .await;
    assert_eq!(done.status, Status::Done, "{:?}", done.error);
    assert!(done.finished_at.is_some_and(|f| f >= at));
    // About a second of 0.2 s segments.
    let segments = done.size.expect("size") / (8 << 10);
    assert!((5..=15).contains(&segments), "{segments}");
    m.close().await;
}
