//! Picking up where a paused, closed or killed manager left off.

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use shardload_manager::{Manager, Status};

/// Where each unfinished part of a download stopped, as its state file says after a clean
/// stop, as the start of a range request (`bytes=1234-`), sorted.
fn stopped_at(state: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(state).expect("state file");
    let state: serde_json::Value = serde_json::from_str(&text).expect("state json");
    let mut stopped: Vec<String> = state["segments"]
        .as_array()
        .expect("segments")
        .iter()
        .filter(|s| s["written"].as_u64() < s["end"].as_u64())
        .map(|s| format!("bytes={}-", s["written"]))
        .collect();
    stopped.sort();
    stopped
}

/// Where the first `parts` requests after the first `skip` started, as [`stopped_at`] has
/// them: a download that goes on from where it stopped asks for each part from there, none
/// from the start of a part.
async fn resumed_from(base: &str, skip: usize, parts: usize) -> Vec<String> {
    let mut resumed: Vec<String> = ranges_asked(base).await[skip..]
        .iter()
        .take(parts)
        .map(|r| format!("{}-", r.split('-').next().unwrap_or_default()))
        .collect();
    resumed.sort();
    resumed
}

/// Waits until the server answers no request: what connections that are gone still sent
/// is counted then.
async fn server_idle(base: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let stats: Vec<serde_json::Value> = reqwest::get(format!("{base}/_stats"))
            .await
            .expect("stats")
            .json()
            .await
            .expect("stats json");
        if stats.iter().all(|u| u["active"].as_u64() == Some(0)) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the server still sends"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_keeps_progress_and_start_resumes() {
    let base = start_server().await;
    let w = WorkDir::new("pause");
    let m = Manager::open(&w.db(), w.config(3)).await.expect("open");
    let size = 8 * MB;
    // 4 connections at 1 MiB/s each: about 2 s in total.
    let task = m
        .add(new_task(format!(
            "{base}/file/p.bin?size={size}&seed=2&rate=1M"
        )))
        .await
        .expect("add");
    wait_for(&m, task.id, 10, |t| t.received > 0).await;
    m.pause(task.id).await.expect("pause");
    let paused = m.get(task.id).await.expect("get");
    assert_eq!(paused.status, Status::Paused);
    assert!(paused.received > 0 && paused.received < size);
    let stopped = stopped_at(&w.path().join("downloads/p.bin.shardload-state"));
    let asked_before = ranges_asked(&base).await.len();

    m.start(task.id).await.expect("start");
    let done = wait_done(&m, task.id).await;
    assert_file(&done, 2, size);
    assert_eq!(
        resumed_from(&base, asked_before, stopped.len()).await,
        stopped
    );
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn started_once_and_its_speeds_go_on_after_a_pause() {
    let base = start_server().await;
    let w = WorkDir::new("speeds");
    let m = Manager::open(&w.db(), w.config(3)).await.expect("open");
    let size = 16 * MB;
    // 4 connections at 1 MiB/s each: about 4 s in total.
    let task = m
        .add(new_task(format!(
            "{base}/file/s.bin?size={size}&seed=5&rate=1M"
        )))
        .await
        .expect("add");
    let started = wait_for(&m, task.id, 10, |t| t.started_at.is_some())
        .await
        .started_at;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while m
        .speed_history(task.id)
        .await
        .expect("speeds")
        .is_none_or(|h| h.seconds() < 2)
    {
        assert!(tokio::time::Instant::now() < deadline, "no speeds kept");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    m.pause(task.id).await.expect("pause");
    let paused = m
        .speed_history(task.id)
        .await
        .expect("speeds")
        .expect("saved when paused");
    assert!(paused.seconds() >= 2, "{paused:?}");
    assert!(paused.points.iter().any(|&p| p > 0), "{paused:?}");

    m.start(task.id).await.expect("start");
    let done = wait_done(&m, task.id).await;
    assert_eq!(
        done.started_at, started,
        "a resume keeps when it first began"
    );
    assert!(done.finished_at >= started);
    let whole = m
        .speed_history(task.id)
        .await
        .expect("speeds")
        .expect("saved when done");
    assert!(whole.seconds() > paused.seconds(), "{whole:?}");
    assert!(whole.points.starts_with(&paused.points), "{whole:?}");
    m.close().await;

    // Still there after the list is opened again.
    let m = Manager::open(&w.db(), w.config(3)).await.expect("reopen");
    assert_eq!(m.speed_history(task.id).await.expect("speeds"), Some(whole));
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn close_then_open_resumes() {
    let base = start_server().await;
    let w = WorkDir::new("close");
    // 4 connections at 1 MiB/s: about 6 s, so the first checkpoint (after 2 s) comes well
    // before the end, however slow the start.
    let size = 24 * MB;
    let url = format!("{base}/file/c.bin?size={size}&seed=4&rate=1M");
    let id = {
        let m = Manager::open(&w.db(), w.config(3)).await.expect("open");
        let task = m.add(new_task(url)).await.expect("add");
        // The state file is written every 2 s.
        let state = w.path().join("downloads/c.bin.shardload-state");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while checkpointed(&state) < 2 * MB {
            assert!(tokio::time::Instant::now() < deadline, "no checkpoint");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        m.close().await;
        task.id
    };
    let stopped = stopped_at(&w.path().join("downloads/c.bin.shardload-state"));
    let asked_before = ranges_asked(&base).await.len();
    let m = Manager::open(&w.db(), w.config(3)).await.expect("reopen");
    let done = wait_done(&m, id).await;
    assert_file(&done, 4, size);
    assert_eq!(
        resumed_from(&base, asked_before, stopped.len()).await,
        stopped
    );
    m.close().await;
}

/// Bytes the state file at `state` records as written, 0 while there is none.
fn checkpointed(state: &Path) -> u64 {
    let Ok(text) = std::fs::read_to_string(state) else {
        return 0;
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|s| s["segments"].as_array().cloned())
        .into_iter()
        .flatten()
        .map(|s| s["written"].as_u64().unwrap_or(0) - s["start"].as_u64().unwrap_or(0))
        .sum()
}

/// Bytes a download resuming from `state` after a kill fetches: each part's rest, and its
/// last seconds again (the engine's `State::rewind`: 2 s at the part's speed, at least
/// 1 MiB).
fn to_fetch(state: &Path) -> u64 {
    let text = std::fs::read_to_string(state).expect("state file");
    let state: serde_json::Value = serde_json::from_str(&text).expect("state json");
    let field = |s: &serde_json::Value, k: &str| s[k].as_u64().unwrap_or(0);
    state["segments"]
        .as_array()
        .expect("segments")
        .iter()
        .map(|s| {
            let back = (field(s, "speed") * 2).max(MB);
            let kept = field(s, "written")
                .saturating_sub(back)
                .max(field(s, "start"));
            field(s, "end") - kept
        })
        .sum()
}

/// The process dies mid-download (here: its runtime is dropped, nothing is closed). The next
/// open finds the task still marked downloading and resumes from the state file.
#[tokio::test(flavor = "multi_thread")]
async fn killed_manager_resumes_on_next_open() {
    let base = start_server().await;
    let w = WorkDir::new("killed");
    // 4 connections at 1 MiB/s: about 8 s; killed once 20 MiB are checkpointed.
    let size = 32 * MB;
    let url = format!("{base}/file/k.bin?size={size}&seed=5&rate=1M");
    let (db, config) = (w.db(), w.config(3));
    let state = w.path().join("downloads/k.bin.shardload-state");
    let id = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let id = rt.block_on(async {
            let m = Manager::open(&db, config).await.expect("open");
            let task = m.add(new_task(url)).await.expect("add");
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while checkpointed(&state) < 20 * MB {
                assert!(tokio::time::Instant::now() < deadline, "no checkpoint");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert_eq!(
                m.get(task.id).await.expect("get").status,
                Status::Downloading
            );
            task.id
        });
        rt.shutdown_timeout(Duration::ZERO);
        id
    })
    .join()
    .expect("join");
    assert!(w.path().join("downloads/k.bin.shardload-state").exists());
    let expected = to_fetch(&w.path().join("downloads/k.bin.shardload-state"));
    server_idle(&base).await;
    let before = bytes_sent(&base).await;

    let m = Manager::open(&w.db(), w.config(3)).await.expect("reopen");
    let done = wait_done(&m, id).await;
    assert_file(&done, 5, size);
    // Starting over would fetch the whole file again. Resuming fetches what the state file
    // left out, and each part's last seconds; connections whose ranges move when parts are
    // split leave what their socket buffers hold, a little more.
    let again = bytes_sent(&base).await - before;
    assert!(
        (expected..expected + 2 * MB).contains(&again) && expected < size,
        "fetched {again} bytes again, {expected} expected, of {size}"
    );
    m.close().await;
}
