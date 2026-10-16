//! The download list against the test server: adding, pausing, removing, the queue, and
//! picking up where a closed or killed manager left off.

use std::path::{Path, PathBuf};
use std::time::Duration;

use idma_manager::{Category, Config, Event, Manager, NewTask, Status, Task, TaskId};
use idma_testserver::{Verdict, router, verify_file};

const MB: u64 = 1 << 20;

async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, router()).await });
    format!("http://{addr}")
}

/// A fresh folder for one test, removed when the test ends.
struct WorkDir(PathBuf);

impl WorkDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir()
            .join("idma-manager-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create workdir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn db(&self) -> PathBuf {
        self.0.join("list.db")
    }

    fn config(&self, max_active: usize) -> Config {
        Config {
            dir: self.0.join("downloads"),
            max_active,
            connections: 4,
            ..Config::default()
        }
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn new_task(url: String) -> NewTask {
    NewTask {
        url,
        start: true,
        ..NewTask::default()
    }
}

/// Polls until `pred` holds for the task, or panics after `secs`.
async fn wait_for(m: &Manager, id: TaskId, secs: u64, pred: impl Fn(&Task) -> bool) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let t = m.get(id).await.expect("get");
        if pred(&t) {
            return t;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out; task is {:?} ({:?})",
            t.status,
            t.error
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_done(m: &Manager, id: TaskId) -> Task {
    let t = wait_for(m, id, 60, |t| {
        matches!(t.status, Status::Done | Status::Error)
    })
    .await;
    assert_eq!(t.status, Status::Done, "{:?}", t.error);
    t
}

fn assert_file(t: &Task, seed: u64, size: u64) {
    let path = t.path.as_ref().expect("path");
    assert_eq!(verify_file(path, seed, size).expect("verify"), Verdict::Ok);
}

async fn bytes_sent(base: &str) -> u64 {
    let stats: Vec<serde_json::Value> = reqwest::get(format!("{base}/_stats"))
        .await
        .expect("stats")
        .json()
        .await
        .expect("stats json");
    stats.iter().filter_map(|u| u["bytes_sent"].as_u64()).sum()
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_under_the_server_name_and_reports_progress() {
    let base = start_server().await;
    let w = WorkDir::new("basic");
    let m = Manager::open(&w.db(), w.config(3)).await.expect("open");
    let mut events = m.subscribe();
    let size = 2 * MB;
    let task = m
        .add(new_task(format!(
            "{base}/file/x.pdf?size={size}&seed=1&rate=1M&cd=%C3%A7ay.zip"
        )))
        .await
        .expect("add");
    // Guessed from the URL first, then taken from the server's name.
    assert_eq!(task.category, Category::Documents);
    let done = wait_done(&m, task.id).await;
    assert_eq!(done.name.as_deref(), Some("çay.zip"));
    assert_eq!(done.category, Category::Compressed);
    assert_eq!(done.size, Some(size));
    assert_eq!(done.received, size);
    assert!(done.finished_at.is_some());
    assert_file(&done, 1, size);

    let (mut added, mut progress, mut finished) = (false, 0, false);
    while let Ok(e) = events.try_recv() {
        match e {
            Event::Added(t) => added |= t.id == task.id,
            Event::Progress(p) if p.id == task.id => progress += 1,
            Event::Updated(t) if t.id == task.id && t.status == Status::Done => finished = true,
            _ => {}
        }
    }
    assert!(added && finished, "added {added}, finished {finished}");
    assert!(progress >= 2, "{progress} progress events");
    m.close().await;
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

    m.start(task.id).await.expect("start");
    let done = wait_done(&m, task.id).await;
    assert_file(&done, 2, size);
    let sent = bytes_sent(&base).await;
    assert!(sent < size + 4 * MB, "server sent {} MiB", sent / MB);
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_stops_and_deletes_files() {
    let base = start_server().await;
    let w = WorkDir::new("remove");
    let m = Manager::open(&w.db(), w.config(3)).await.expect("open");
    let mut events = m.subscribe();
    let task = m
        .add(new_task(format!(
            "{base}/file/r.bin?size={}&seed=3&rate=256K",
            8 * MB
        )))
        .await
        .expect("add");
    let running = wait_for(&m, task.id, 10, |t| t.received > 0 || t.path.is_some()).await;
    let path = running.path.expect("path");
    m.remove(task.id, true).await.expect("remove");
    assert!(matches!(
        m.get(task.id).await,
        Err(idma_manager::Error::NotFound(_))
    ));
    for suffix in ["", ".idma-part", ".idma-state", ".idma-lock"] {
        let p = PathBuf::from(format!("{}{suffix}", path.display()));
        assert!(!p.exists(), "{} left behind", p.display());
    }
    let mut removed = false;
    while let Ok(e) = events.try_recv() {
        removed |= matches!(e, Event::Removed(id) if id == task.id);
    }
    assert!(removed);
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_runs_at_most_max_active() {
    let base = start_server().await;
    let w = WorkDir::new("queue");
    let m = Manager::open(&w.db(), w.config(2)).await.expect("open");
    let size = MB;
    let mut ids = Vec::new();
    for seed in 0..6 {
        let t = m
            .add(new_task(format!(
                "{base}/file/q{seed}.bin?size={size}&seed={seed}&rate=1M"
            )))
            .await
            .expect("add");
        ids.push((t.id, seed));
    }
    let mut most = 0;
    loop {
        let list = m.list().await.expect("list");
        let active = list
            .iter()
            .filter(|t| t.status == Status::Downloading)
            .count();
        most = most.max(active);
        assert!(active <= 2, "{active} running at once");
        if list.iter().all(|t| t.status == Status::Done) {
            break;
        }
        assert!(
            list.iter().all(|t| t.status != Status::Error),
            "{:?}",
            list.iter().find_map(|t| t.error.clone())
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(most, 2);
    for (id, seed) in ids {
        assert_file(&m.get(id).await.expect("get"), seed, size);
    }
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn twenty_downloads_at_once() {
    let base = start_server().await;
    let w = WorkDir::new("twenty");
    let m = Manager::open(&w.db(), w.config(20)).await.expect("open");
    let size = 512 << 10;
    let mut ids = Vec::new();
    for seed in 0..20 {
        // Same server name for all: every copy after the first gets a number.
        let t = m
            .add(new_task(format!(
                "{base}/file/same.bin?size={size}&seed={seed}"
            )))
            .await
            .expect("add");
        ids.push((t.id, seed));
    }
    let mut paths = std::collections::HashSet::new();
    for (id, seed) in ids {
        let t = wait_done(&m, id).await;
        assert_file(&t, seed, size);
        assert!(
            paths.insert(t.path.clone().expect("path")),
            "two tasks wrote one file"
        );
    }
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn close_then_open_resumes() {
    let base = start_server().await;
    let w = WorkDir::new("close");
    let size = 8 * MB;
    let url = format!("{base}/file/c.bin?size={size}&seed=4&rate=1M");
    let id = {
        let m = Manager::open(&w.db(), w.config(3)).await.expect("open");
        let task = m.add(new_task(url)).await.expect("add");
        // The state file is written every 2 s.
        wait_for(&m, task.id, 10, |t| t.received >= 2 * MB).await;
        tokio::time::sleep(Duration::from_millis(2200)).await;
        m.close().await;
        task.id
    };
    let m = Manager::open(&w.db(), w.config(3)).await.expect("reopen");
    let done = wait_done(&m, id).await;
    assert_file(&done, 4, size);
    let sent = bytes_sent(&base).await;
    assert!(sent < size + 4 * MB, "server sent {} MiB", sent / MB);
    m.close().await;
}

/// The process dies mid-download (here: its runtime is dropped, nothing is closed). The next
/// open finds the task still marked downloading and resumes from the state file.
#[tokio::test(flavor = "multi_thread")]
async fn killed_manager_resumes_on_next_open() {
    let base = start_server().await;
    let w = WorkDir::new("killed");
    // 4 connections at 1 MiB/s: about 4 s; killed after the first checkpoint (2 s).
    let size = 16 * MB;
    let url = format!("{base}/file/k.bin?size={size}&seed=5&rate=1M");
    let (db, config) = (w.db(), w.config(3));
    let id = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let id = rt.block_on(async {
            let m = Manager::open(&db, config).await.expect("open");
            let task = m.add(new_task(url)).await.expect("add");
            tokio::time::sleep(Duration::from_millis(2500)).await;
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
    assert!(w.path().join("downloads/k.bin.idma-state").exists());
    let before = bytes_sent(&base).await;

    let m = Manager::open(&w.db(), w.config(3)).await.expect("reopen");
    let done = wait_done(&m, id).await;
    assert_file(&done, 5, size);
    // Starting over would fetch the whole file again; at least the first checkpoint's
    // worth (2 s at up to 4 MiB/s, minus ramp-up) was kept.
    let again = bytes_sent(&base).await - before;
    assert!(
        again < size - 2 * MB,
        "fetched {} of {} MiB again",
        again / MB,
        size / MB
    );
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_checksum_is_refused_and_failures_are_recorded() {
    let base = start_server().await;
    let w = WorkDir::new("errors");
    let m = Manager::open(&w.db(), w.config(3)).await.expect("open");
    let mut bad = new_task(format!("{base}/file/a.bin?size=1000"));
    bad.checksum = Some("sha256:abc".into());
    assert!(matches!(
        m.add(bad).await,
        Err(idma_manager::Error::BadChecksum(_))
    ));

    let task = m
        .add(new_task(format!("{base}/no-such-route/gone.bin")))
        .await
        .expect("add");
    let t = wait_for(&m, task.id, 10, |t| t.status == Status::Error).await;
    assert!(
        t.error.as_deref().is_some_and(|e| e.contains("404")),
        "{:?}",
        t.error
    );
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_downloads_again_from_scratch() {
    let base = start_server().await;
    let w = WorkDir::new("restart");
    let m = Manager::open(&w.db(), w.config(3)).await.expect("open");
    let size = MB;
    let url = format!("{base}/file/again.bin?size={size}&seed=8");
    let probe = m.probe(&url, &[]).await.expect("probe");
    assert_eq!(probe.name, "again.bin");
    assert_eq!(probe.size, Some(size));

    let task = m.add(new_task(url)).await.expect("add");
    let first = wait_done(&m, task.id).await;
    let before = bytes_sent(&base).await;
    m.restart(task.id).await.expect("restart");
    let second = wait_done(&m, task.id).await;
    assert_eq!(second.path, first.path, "same file, not a numbered copy");
    assert_file(&second, 8, size);
    assert!(
        bytes_sent(&base).await - before >= size,
        "nothing was downloaded again"
    );
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refreshed_link_continues_only_for_the_same_file() {
    let base = start_server().await;
    let w = WorkDir::new("refresh");
    let m = Manager::open(&w.db(), w.config(3)).await.expect("open");
    // 4 connections at 1 MiB/s: about 8 s, so it is still running when the link changes.
    let size = 32 * MB;
    let task = m
        .add(new_task(format!(
            "{base}/file/old.bin?size={size}&seed=21&rate=1M"
        )))
        .await
        .expect("add");
    wait_for(&m, task.id, 10, |t| t.received > 0).await;
    // Let the state file record some progress.
    tokio::time::sleep(Duration::from_millis(2200)).await;

    let other = format!("{base}/file/new.bin?size={size}&seed=22&rate=1M");
    let refused = m.refresh_link(task.id, &other).await;
    assert!(
        matches!(
            refused,
            Err(idma_manager::Error::Engine(idma_engine::Error::FileChanged))
        ),
        "{refused:?}"
    );
    let t = m.get(task.id).await.expect("get");
    assert!(t.url.contains("old.bin"), "{}", t.url);
    assert!(
        matches!(
            t.status,
            Status::Downloading | Status::Queued | Status::Done
        ),
        "{:?}",
        t.status
    );

    let fresh = format!("{base}/file/fresh.bin?size={size}&seed=21&rate=1M");
    m.refresh_link(task.id, &fresh).await.expect("refresh");
    let done = wait_done(&m, task.id).await;
    assert_eq!(done.url, fresh);
    assert_eq!(done.name.as_deref(), Some("old.bin"), "keeps its file");
    assert_file(&done, 21, size);
    m.close().await;
}
