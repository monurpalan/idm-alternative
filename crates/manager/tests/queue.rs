//! Queue order, starting and pausing everything, and automatic retries.

use std::path::PathBuf;
use std::time::Duration;

use idma_manager::{Config, Manager, Move, NewTask, Status, Task, TaskId};
use idma_testserver::{Verdict, router, verify_file};

const MB: u64 = 1 << 20;

async fn serve_on(listener: tokio::net::TcpListener) {
    axum::serve(listener, router()).await.ok();
}

async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(serve_on(listener));
    format!("http://{addr}")
}

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

    fn db(&self) -> PathBuf {
        self.0.join("list.db")
    }

    fn config(&self) -> Config {
        Config {
            dir: self.0.join("downloads"),
            max_active: 3,
            connections: 4,
            retry_delay: Duration::from_millis(200),
            ..Config::default()
        }
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn task(url: String, start: bool) -> NewTask {
    NewTask {
        url,
        start,
        ..NewTask::default()
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
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Ids of the unfinished downloads in queue order.
async fn queue(m: &Manager) -> Vec<TaskId> {
    m.list()
        .await
        .expect("list")
        .iter()
        .filter(|t| t.status != Status::Done)
        .map(|t| t.id)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn network_failures_are_retried_until_the_server_answers() {
    // A port with nothing listening yet.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);

    let w = WorkDir::new("retry");
    let m = Manager::open(&w.db(), w.config()).await.expect("open");
    let size = MB;
    let t = m
        .add(task(
            format!("http://{addr}/file/late.bin?size={size}&seed=31"),
            true,
        ))
        .await
        .expect("add");

    let waiting = wait_for(&m, t.id, 10, |t| {
        t.error.as_deref().is_some_and(|e| e.contains("retrying"))
    })
    .await;
    assert_eq!(waiting.status, Status::Queued);

    let listener = tokio::net::TcpListener::bind(addr).await.expect("rebind");
    tokio::spawn(serve_on(listener));
    let done = wait_for(&m, t.id, 30, |t| t.status == Status::Done).await;
    assert_eq!(done.error, None);
    let path = done.path.expect("path");
    assert_eq!(verify_file(&path, 31, size).expect("verify"), Verdict::Ok);
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn gives_up_after_the_last_retry() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);

    let w = WorkDir::new("give-up");
    let config = Config {
        max_retries: 2,
        retry_delay: Duration::from_millis(50),
        ..w.config()
    };
    let m = Manager::open(&w.db(), config).await.expect("open");
    let t = m
        .add(task(
            format!("http://{addr}/file/never.bin?size=1000"),
            true,
        ))
        .await
        .expect("add");
    let failed = wait_for(&m, t.id, 20, |t| t.status == Status::Error).await;
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|e| !e.contains("retrying")),
        "{:?}",
        failed.error
    );
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_files_are_not_retried() {
    let base = start_server().await;
    let w = WorkDir::new("no-retry");
    let m = Manager::open(&w.db(), w.config()).await.expect("open");
    let t = m
        .add(task(format!("{base}/no-such-route/x.bin"), true))
        .await
        .expect("add");
    let failed = wait_for(&m, t.id, 10, |t| t.status == Status::Error).await;
    assert!(failed.error.as_deref().is_some_and(|e| e.contains("404")));
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_move_in_the_queue() {
    let base = start_server().await;
    let w = WorkDir::new("move");
    let m = Manager::open(&w.db(), w.config()).await.expect("open");
    let mut ids = Vec::new();
    for i in 0..4 {
        let t = m
            .add(task(format!("{base}/file/m{i}.bin?size=1000"), false))
            .await
            .expect("add");
        ids.push(t.id);
    }
    let [a, b, c, d] = ids[..] else {
        unreachable!()
    };
    assert_eq!(queue(&m).await, [a, b, c, d]);
    m.move_task(d, Move::Top).await.expect("top");
    assert_eq!(queue(&m).await, [d, a, b, c]);
    m.move_task(a, Move::Bottom).await.expect("bottom");
    assert_eq!(queue(&m).await, [d, b, c, a]);
    m.move_task(c, Move::Up).await.expect("up");
    assert_eq!(queue(&m).await, [d, c, b, a]);
    m.move_task(d, Move::Down).await.expect("down");
    assert_eq!(queue(&m).await, [c, d, b, a]);
    // Already at the ends: nothing changes.
    m.move_task(c, Move::Up).await.expect("up at top");
    m.move_task(a, Move::Down).await.expect("down at bottom");
    assert_eq!(queue(&m).await, [c, d, b, a]);

    // The queue starts them in this order.
    let config = Config {
        max_active: 1,
        ..w.config()
    };
    m.set_config(config).await.expect("config");
    m.start_all().await.expect("start all");
    let first = wait_for(&m, c, 10, |t| t.status != Status::Paused).await;
    assert_ne!(
        first.status,
        Status::Queued,
        "the top download starts first"
    );
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn start_all_and_pause_all() {
    let base = start_server().await;
    let w = WorkDir::new("all");
    let m = Manager::open(&w.db(), w.config()).await.expect("open");
    let mut ids = Vec::new();
    for i in 0..5 {
        let t = m
            .add(task(
                format!("{base}/file/a{i}.bin?size={}&seed={i}&rate=256K", 16 * MB),
                false,
            ))
            .await
            .expect("add");
        ids.push(t.id);
    }
    m.start_all().await.expect("start all");
    let list = m.list().await.expect("list");
    assert_eq!(
        list.iter()
            .filter(|t| t.status == Status::Downloading)
            .count(),
        3
    );
    assert_eq!(
        list.iter().filter(|t| t.status == Status::Queued).count(),
        2
    );

    m.pause_all().await.expect("pause all");
    let list = m.list().await.expect("list");
    assert!(
        list.iter().all(|t| t.status == Status::Paused),
        "{:?}",
        list.iter().map(|t| t.status).collect::<Vec<_>>()
    );
    m.close().await;
}
