//! ffmpeg set up the first time a download needs it (to join a stream's video and audio):
//! its progress shown as the download's, a failure said, a success kept for the next ones.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use shardload_engine::{Phase, Progress};
use shardload_manager::{Event, FfmpegSetup, Manager, NewTask, Status, Stream, Task};

/// A setup that reports half of 100 bytes, then gives `result`, counting its calls.
fn setup(result: Result<PathBuf, String>, calls: &Arc<AtomicUsize>) -> FfmpegSetup {
    let calls = Arc::clone(calls);
    FfmpegSetup(Arc::new(move |_opts, tx, _cancel| {
        calls.fetch_add(1, Ordering::SeqCst);
        let result = result.clone();
        Box::pin(async move {
            tx.send_replace(Progress {
                size: Some(100),
                received: 50,
                ..Progress::default()
            });
            result
        })
    }))
}

/// A stream whose video and audio come as separate playlists.
fn stream(base: &str, seed: u64) -> NewTask {
    let q = format!("segments=3&seg_size=16K&seed={seed}");
    NewTask {
        url: format!("{base}/hls/v720.m3u8?{q}"),
        start: true,
        stream: Some(Stream {
            audio: Some(format!("{base}/hls/a.m3u8?{q}")),
            ..Stream::default()
        }),
        ..NewTask::default()
    }
}

async fn ended(m: &Manager, task: &Task) -> Task {
    wait_for(m, task.id, 60, |t| {
        matches!(t.status, Status::Done | Status::Error)
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_setup_is_said_and_shown_as_the_download_s_progress() {
    let base = start_server().await;
    let w = WorkDir::new("ffmpeg-fails");
    let calls = Arc::new(AtomicUsize::new(0));
    let config = shardload_manager::Config {
        ffmpeg_setup: Some(setup(Err("no route to the server".into()), &calls)),
        ..w.config(1)
    };
    let m = Manager::open(&w.db(), config).await.expect("open");
    let mut events = m.subscribe();
    let task = m.add(stream(&base, 1)).await.expect("add");
    let done = ended(&m, &task).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut setting_up = false;
    while let Ok(e) = events.try_recv() {
        if let Event::Progress(p) = e {
            setting_up |= p.id == task.id && p.phase == Phase::SettingUp && p.received == 50;
        }
    }
    assert!(setting_up, "no progress of the setup");
    // Said as it is, with why, and nothing half-joined left.
    assert_eq!(done.status, Status::Error);
    assert_eq!(
        done.error.as_deref(),
        Some(
            "this stream has separate audio, which ffmpeg joins with the video, and ffmpeg \
             could not be set up: no route to the server"
        )
    );
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_program_set_up_once_serves_the_next_downloads() {
    let base = start_server().await;
    let w = WorkDir::new("ffmpeg-kept");
    let calls = Arc::new(AtomicUsize::new(0));
    let fake = w.path().join("no-such-ffmpeg.exe");
    let config = shardload_manager::Config {
        ffmpeg_setup: Some(setup(Ok(fake), &calls)),
        ..w.config(1)
    };
    let m = Manager::open(&w.db(), config).await.expect("open");
    let first = m.add(stream(&base, 2)).await.expect("add");
    let first = ended(&m, &first).await;
    let second = m.add(stream(&base, 3)).await.expect("add");
    let second = ended(&m, &second).await;
    // Both use the program set up for the first (which is not a real one, so joining fails).
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        (first.status, second.status),
        (Status::Error, Status::Error)
    );
    m.close().await;
}
