//! Disk space for the queue, with made-up drives (no real disk is filled): what the queue
//! needs per drive before it starts, moving downloads to another drive, and pausing running
//! ones before a drive fills up.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use shardload_manager::disk::RESERVE;
use shardload_manager::{Config, ErrorKind, Event, Manager, NewTask, Space, Status, Task, TaskId};

const MB: u64 = 1 << 20;

async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, shardload_testserver::router()).await });
    format!("http://{addr}")
}

/// A work folder with two made-up drives: `small/` (its free space set by the test) and
/// `big/` (100 GB free).
struct Drives {
    root: PathBuf,
    small_free: Arc<AtomicU64>,
}

impl Drives {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir()
            .join("shardload-manager-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).expect("workdir");
        Self {
            root,
            small_free: Arc::new(AtomicU64::new(1 << 30)),
        }
    }

    fn small(&self) -> PathBuf {
        self.root.join("small")
    }

    fn big(&self) -> PathBuf {
        self.root.join("big")
    }

    fn config(&self) -> Config {
        let (small, big, free) = (self.small(), self.big(), Arc::clone(&self.small_free));
        Config {
            dir: self.small(),
            max_active: 2,
            connections: 2,
            engine: shardload_engine::Options {
                find_checksum: false,
                ..shardload_engine::Options::default()
            },
            free_space: Some(Space(Arc::new(move |dir: &Path| {
                if dir.starts_with(&small) {
                    Some((small.clone(), free.load(Ordering::SeqCst)))
                } else if dir.starts_with(&big) {
                    Some((big.clone(), 100 << 30))
                } else {
                    None
                }
            }))),
            disk_watch: Duration::from_millis(100),
            ..Config::default()
        }
    }
}

impl Drop for Drives {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

async fn add(m: &Manager, url: String, size: Option<u64>, start: bool) -> Task {
    m.add(NewTask {
        url,
        size,
        start,
        ..NewTask::default()
    })
    .await
    .expect("add")
}

async fn wait_for(m: &Manager, id: TaskId, what: impl Fn(&Task) -> bool) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let t = m.get(id).await.expect("get");
        if what(&t) {
            return t;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out: {t:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queue_that_does_not_fit_and_moving_part_of_it() {
    let base = start_server().await;
    let d = Drives::new("disk-queue");
    let m = Manager::open(&d.root.join("list.db"), d.config())
        .await
        .expect("open");
    // 1 GB free; 700 + 600 MB to download, and one whose size is not known yet.
    let a = add(&m, format!("{base}/file/a.iso"), Some(700 * MB), false).await;
    let b = add(&m, format!("{base}/file/b.iso"), Some(600 * MB), false).await;
    let c = add(&m, format!("{base}/file/c.iso"), None, false).await;
    let ids = [a.id, b.id, c.id];

    let needs = m.disk_needs(&ids).await.expect("needs");
    assert_eq!(needs.len(), 1);
    let small = &needs[0];
    assert_eq!(small.volume, d.small());
    assert_eq!(small.need, 1300 * MB);
    assert_eq!(small.unknown, 1);
    assert_eq!(small.short(), Some(1300 * MB + RESERVE - (1 << 30)));

    // "Move to another drive": the big one has room for it all.
    std::fs::create_dir_all(d.big()).expect("big");
    m.relocate(b.id, &d.big()).await.expect("move");
    let needs = m.disk_needs(&ids).await.expect("needs");
    assert_eq!(needs.len(), 2);
    let small = needs.iter().find(|n| n.volume == d.small()).expect("small");
    let big = needs.iter().find(|n| n.volume == d.big()).expect("big");
    assert_eq!(
        (small.need, small.short()),
        (700 * MB, Some(700 * MB + RESERVE - (1 << 30)))
    );
    assert_eq!((big.need, big.short()), (600 * MB, None));
    // Enough free on the small one too now.
    d.small_free.store(2 << 30, Ordering::SeqCst);
    let needs = m.disk_needs(&ids).await.expect("needs");
    assert!(needs.iter().all(|n| n.short().is_none()), "{needs:?}");
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_pause_before_the_drive_fills() {
    let base = start_server().await;
    let d = Drives::new("disk-pause");
    std::fs::create_dir_all(d.big()).expect("big");
    let m = Manager::open(&d.root.join("list.db"), d.config())
        .await
        .expect("open");
    let mut events = m.subscribe();
    // Two slow ones on the small drive (one running, one waiting: max_active 2 is taken by
    // the first and the big drive's), one on the big drive.
    let running = add(
        &m,
        format!("{base}/file/r.bin?size=40M&rate=1M&seed=1"),
        None,
        true,
    )
    .await;
    let other = m
        .add(NewTask {
            url: format!("{base}/file/o.bin?size=40M&rate=1M&seed=2"),
            dir: Some(d.big()),
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");
    let waiting = add(
        &m,
        format!("{base}/file/w.bin?size=40M&rate=1M&seed=3"),
        None,
        true,
    )
    .await;
    wait_for(&m, running.id, |t| {
        t.received > 0 || t.status == Status::Downloading
    })
    .await;
    wait_for(&m, other.id, |t| t.status == Status::Downloading).await;
    assert_eq!(m.get(waiting.id).await.expect("get").status, Status::Queued);

    // The small drive is down to 400 MB: below the reserve.
    d.small_free.store(400 * MB, Ordering::SeqCst);
    let paused = wait_for(&m, running.id, |t| t.status == Status::Paused).await;
    assert_eq!(paused.error_kind, Some(ErrorKind::Disk));
    assert!(
        paused
            .error
            .as_deref()
            .is_some_and(|e| e.contains("400 MB left")),
        "{paused:?}"
    );
    let w = wait_for(&m, waiting.id, |t| t.status == Status::Paused).await;
    assert_eq!(
        w.error_kind,
        Some(ErrorKind::Disk),
        "the one waiting to start there too"
    );
    let low = loop {
        match tokio::time::timeout(Duration::from_secs(5), events.recv()).await {
            Ok(Ok(Event::DiskLow(low))) => break low,
            Ok(Ok(_)) => {}
            other => panic!("no disk event: {other:?}"),
        }
    };
    assert_eq!(low.volume, d.small());
    assert_eq!(low.free, 400 * MB);
    assert!(low.paused.contains(&running.id) && low.paused.contains(&waiting.id));
    // The other drive's download goes on.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        m.get(other.id).await.expect("get").status,
        Status::Downloading
    );

    // Space freed: resumed by the user, it goes on from where it was.
    d.small_free.store(10 << 30, Ordering::SeqCst);
    let before = m.get(running.id).await.expect("get").received;
    m.start(running.id).await.expect("start");
    let t = wait_for(&m, running.id, |t| t.status == Status::Downloading).await;
    assert!(t.received >= before);
    assert_eq!(t.error_kind, None);
    m.close().await;
}
