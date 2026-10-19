//! Renaming and moving downloads in every state: before they start, waiting in the queue,
//! paused with part of the file, while downloading, and finished.

use std::path::{Path, PathBuf};
use std::time::Duration;

use shardload_manager::{Config, Manager, NewTask, Status, Task, TaskId};
use shardload_testserver::{Verdict, router, verify_file};

const MB: u64 = 1 << 20;

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
            .join("shardload-relocate-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create workdir");
        Self(dir)
    }

    fn downloads(&self) -> PathBuf {
        self.0.join("downloads")
    }

    fn other(&self) -> PathBuf {
        self.0.join("other")
    }

    async fn open(&self, max_active: usize) -> Manager {
        let config = Config {
            dir: self.downloads(),
            max_active,
            connections: 4,
            engine: shardload_engine::Options {
                find_checksum: false,
                ..shardload_engine::Options::default()
            },
            ..Config::default()
        };
        Manager::open(&self.0.join("list.db"), config)
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

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", path.display()))
}

/// The engine's partial data and state are where the download's path says.
fn assert_partial_at(path: &Path) {
    assert!(
        sibling(path, ".shardload-part").exists(),
        "{}",
        path.display()
    );
    assert!(
        sibling(path, ".shardload-state").exists(),
        "{}",
        path.display()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn before_it_starts_only_the_list_changes() {
    let base = start_server().await;
    let w = WorkDir::new("not-started");
    let m = w.open(3).await;
    let add = |seed: u64| NewTask {
        url: format!("{base}/file/a{seed}.bin?size={}&seed={seed}", MB),
        start: false,
        ..NewTask::default()
    };

    // Same folder, a new name (cleaned like a server's).
    let a = m.add(add(1)).await.expect("add");
    let renamed = m.rename(a.id, "my: file?.pdf").await.expect("rename");
    assert_eq!(renamed.name.as_deref(), Some("my_ file_.pdf"));
    assert_eq!(renamed.status, Status::Paused);
    assert!(renamed.path.is_none());
    assert_eq!(renamed.category, shardload_manager::Category::Documents);

    // Another folder.
    let b = m.add(add(2)).await.expect("add");
    let moved = m.relocate(b.id, &w.other()).await.expect("relocate");
    assert_eq!(moved.dir, w.other());
    assert!(moved.path.is_none());

    m.start(a.id).await.expect("start");
    m.start(b.id).await.expect("start");
    let a = wait_done(&m, a.id).await;
    let b = wait_done(&m, b.id).await;
    assert_eq!(a.path, Some(w.downloads().join("my_ file_.pdf")));
    assert_eq!(b.path, Some(w.other().join("a2.bin")));
    assert_file(&a, 1, MB);
    assert_file(&b, 2, MB);
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queued_download_starts_in_its_new_place() {
    let base = start_server().await;
    let w = WorkDir::new("queued");
    // One at a time: the second waits in the queue behind the first.
    let m = w.open(1).await;
    let first = m
        .add(NewTask {
            url: format!("{base}/file/first.bin?size={}&seed=3&rate=2M", 2 * MB),
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");
    let mut queued = Vec::new();
    for seed in [4, 5] {
        let t = m
            .add(NewTask {
                url: format!("{base}/file/q{seed}.bin?size={}&seed={seed}", MB),
                start: true,
                ..NewTask::default()
            })
            .await
            .expect("add");
        assert_eq!(t.status, Status::Queued);
        queued.push(t.id);
    }
    let renamed = m.rename(queued[0], "renamed.bin").await.expect("rename");
    assert_eq!(renamed.status, Status::Queued);
    let moved = m.relocate(queued[1], &w.other()).await.expect("relocate");
    assert_eq!(moved.status, Status::Queued);

    wait_done(&m, first.id).await;
    let a = wait_done(&m, queued[0]).await;
    let b = wait_done(&m, queued[1]).await;
    assert_eq!(a.path, Some(w.downloads().join("renamed.bin")));
    assert_eq!(b.path, Some(w.other().join("q5.bin")));
    assert_file(&a, 4, MB);
    assert_file(&b, 5, MB);
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_download_takes_its_partial_data_along() {
    let base = start_server().await;
    let w = WorkDir::new("paused");
    let m = w.open(3).await;
    let size = 8 * MB;
    let task = m
        .add(NewTask {
            url: format!("{base}/file/p.bin?size={size}&seed=6&rate=1M"),
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");
    wait_for(&m, task.id, 10, |t| t.received > MB).await;
    m.pause(task.id).await.expect("pause");
    let paused = m.get(task.id).await.expect("get");
    let old = paused.path.clone().expect("path");
    assert_partial_at(&old);

    // Renamed in its folder, then moved to another: the data follows both times.
    let renamed = m.rename(task.id, "q.bin").await.expect("rename");
    assert_eq!(renamed.status, Status::Paused);
    let path = renamed.path.clone().expect("path");
    assert_eq!(path, w.downloads().join("q.bin"));
    assert_partial_at(&path);
    assert!(!sibling(&old, ".shardload-part").exists());

    let moved = m.relocate(task.id, &w.other()).await.expect("relocate");
    let path = moved.path.clone().expect("path");
    assert_eq!(path, w.other().join("q.bin"));
    assert_partial_at(&path);
    assert_eq!(moved.received, paused.received);

    m.start(task.id).await.expect("start");
    let done = wait_done(&m, task.id).await;
    assert_eq!(done.path, Some(w.other().join("q.bin")));
    assert_file(&done, 6, size);
    let sent = bytes_sent(&base).await;
    assert!(sent < size + 4 * MB, "server sent {} MiB", sent / MB);
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn moving_a_running_download_resumes_where_it_stopped() {
    let base = start_server().await;
    let w = WorkDir::new("running");
    let m = w.open(3).await;
    let size = 8 * MB;
    let task = m
        .add(NewTask {
            url: format!("{base}/file/r.bin?size={size}&seed=7&rate=1M"),
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");
    wait_for(&m, task.id, 10, |t| t.received > MB).await;
    let old = m.get(task.id).await.expect("get").path.expect("path");

    let moved = m.relocate(task.id, &w.other()).await.expect("relocate");
    // Back in the queue, so it continues by itself.
    assert!(
        matches!(moved.status, Status::Queued | Status::Downloading),
        "{:?}",
        moved.status
    );
    assert_eq!(moved.path, Some(w.other().join("r.bin")));
    assert!(!sibling(&old, ".shardload-part").exists());

    // Renamed while running, in its new folder.
    wait_for(&m, task.id, 10, |t| t.status == Status::Downloading).await;
    let renamed = m.rename(task.id, "s.bin").await.expect("rename");
    assert_eq!(renamed.path, Some(w.other().join("s.bin")));

    let done = wait_done(&m, task.id).await;
    assert_eq!(done.path, Some(w.other().join("s.bin")));
    assert_file(&done, 7, size);
    assert!(!old.exists());
    let sent = bytes_sent(&base).await;
    assert!(sent < size + 4 * MB, "server sent {} MiB", sent / MB);
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_file_moves_and_a_taken_name_gets_a_number() {
    let base = start_server().await;
    let w = WorkDir::new("done");
    let m = w.open(3).await;
    let task = m
        .add(NewTask {
            url: format!("{base}/file/d.bin?size={}&seed=8", MB),
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");
    let done = wait_done(&m, task.id).await;
    let old = done.path.clone().expect("path");

    // In the same folder: the new name is taken by another file.
    std::fs::write(w.downloads().join("e.bin"), b"someone else's").expect("write");
    let renamed = m.rename(task.id, "e.bin").await.expect("rename");
    assert_eq!(renamed.status, Status::Done);
    assert_eq!(renamed.name.as_deref(), Some("e (1).bin"));
    assert_file(&renamed, 8, MB);
    assert!(!old.exists());
    assert_eq!(
        std::fs::read(w.downloads().join("e.bin")).expect("read"),
        b"someone else's"
    );

    // Only the case changes: the same file, not a numbered copy.
    let cased = m.rename(task.id, "E (1).BIN").await.expect("rename");
    assert_eq!(cased.name.as_deref(), Some("E (1).BIN"));
    assert_file(&cased, 8, MB);

    // To another folder where the name is taken too.
    std::fs::create_dir_all(w.other()).expect("mkdir");
    std::fs::write(w.other().join("E (1).BIN"), b"x").expect("write");
    let moved = m.relocate(task.id, &w.other()).await.expect("relocate");
    assert_eq!(moved.path, Some(w.other().join("E (1) (1).BIN")));
    assert_file(&moved, 8, MB);

    // Nothing to do when nothing changes.
    let same = m.relocate(task.id, &w.other()).await.expect("relocate");
    assert_eq!(same.path, moved.path);

    // A finished file that is gone cannot be moved; the list keeps its place.
    std::fs::remove_file(moved.path.as_ref().expect("path")).expect("delete");
    let err = m.relocate(task.id, &w.downloads()).await.expect_err("gone");
    assert!(err.to_string().contains("no longer there"), "{err}");
    assert_eq!(m.get(task.id).await.expect("get").path, moved.path);
    m.close().await;
}
