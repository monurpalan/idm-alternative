//! Downloads with a deadline against the test server: the earliest deadline starts first, a
//! download without one makes room, one falling behind is said early, and the link's
//! lifetime counts as a deadline.

use std::path::PathBuf;
use std::time::Duration;

use shardload_manager::deadline::{Source, Standing};
use shardload_manager::{Config, Event, Manager, NewTask, Status, Task, TaskId};

async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, shardload_testserver::router()).await });
    format!("http://{addr}")
}

async fn open(name: &str, max_active: usize) -> (Manager, PathBuf) {
    let dir = std::env::temp_dir()
        .join("shardload-manager-tests")
        .join(format!("deadline-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("workdir");
    let m = Manager::open(
        &dir.join("list.db"),
        Config {
            dir: dir.join("downloads"),
            max_active,
            connections: 1,
            engine: shardload_engine::Options {
                find_checksum: false,
                check_structure: false,
                ..shardload_engine::Options::default()
            },
            ..Config::default()
        },
    )
    .await
    .expect("open");
    (m, dir)
}

fn in_secs(s: i64) -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
        + s * 1000
}

async fn add(m: &Manager, url: String, deadline: Option<i64>, start: bool) -> Task {
    m.add(NewTask {
        url,
        start,
        deadline,
        ..NewTask::default()
    })
    .await
    .expect("add")
}

async fn wait(m: &Manager, id: TaskId, what: &str, pred: impl Fn(&Task) -> bool) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let t = m.get(id).await.expect("get");
        if pred(&t) {
            return t;
        }
        assert!(tokio::time::Instant::now() < deadline, "{what}: {t:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_earliest_deadline_starts_first() {
    let server = serve().await;
    let (m, dir) = open("order", 1).await;
    let mut events = m.subscribe();
    // Added paused in this order; started together.
    let none = add(
        &m,
        format!("{server}/file/none.bin?size=256K&rate=1M"),
        None,
        false,
    )
    .await;
    let later = add(
        &m,
        format!("{server}/file/later.bin?size=256K&rate=1M"),
        Some(in_secs(600)),
        false,
    )
    .await;
    let sooner = add(
        &m,
        format!("{server}/file/sooner.bin?size=256K&rate=1M"),
        Some(in_secs(300)),
        false,
    )
    .await;
    m.start_all().await.expect("start");
    let mut order = Vec::new();
    while order.len() < 3 {
        if let Ok(Ok(Event::Updated(t))) =
            tokio::time::timeout(Duration::from_secs(30), events.recv()).await
            && t.status == Status::Downloading
            && !order.contains(&t.id)
        {
            order.push(t.id);
        }
    }
    assert_eq!(order, [sooner.id, later.id, none.id]);
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_without_a_deadline_makes_room() {
    let server = serve().await;
    let (m, dir) = open("room", 1).await;
    let plain = add(
        &m,
        format!("{server}/file/plain.bin?size=8M&rate=1M"),
        None,
        true,
    )
    .await;
    wait(&m, plain.id, "it starts", |t| {
        t.status == Status::Downloading
    })
    .await;
    let urgent = add(
        &m,
        format!("{server}/file/urgent.bin?size=1M"),
        Some(in_secs(60)),
        true,
    )
    .await;
    let held = wait(&m, plain.id, "it makes room", |t| {
        t.status == Status::Queued
    })
    .await;
    assert!(
        held.error
            .as_deref()
            .is_some_and(|e| e.contains("deadline")),
        "{held:?}"
    );
    wait(&m, urgent.id, "the urgent one finishes", |t| {
        t.status == Status::Done
    })
    .await;
    wait(&m, plain.id, "then the other goes on", |t| {
        t.status == Status::Done
    })
    .await;
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn falling_behind_is_said_early() {
    let server = serve().await;
    let (m, dir) = open("risk", 2).await;
    let mut events = m.subscribe();
    // 4 MiB at 256 KiB/s takes 16 s; it has 6.
    let t = add(
        &m,
        format!("{server}/file/slow.bin?size=4M&rate=256K"),
        Some(in_secs(6)),
        true,
    )
    .await;
    let said = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Event::DeadlineRisk(r)) = events.recv().await {
                return r;
            }
        }
    })
    .await
    .expect("said within seconds, not at the deadline");
    assert_eq!(said.id, t.id);
    assert!(said.needed > said.speed, "{said:?}");
    let views = m.deadlines().await.expect("deadlines");
    let v = views.iter().find(|v| v.id == t.id).expect("listed");
    assert_eq!((v.source, v.standing), (Source::Own, Standing::AtRisk));
    // Said once, not on every look.
    tokio::time::sleep(Duration::from_secs(2)).await;
    while let Ok(e) = events.try_recv() {
        assert!(!matches!(e, Event::DeadlineRisk(_)), "said again");
    }
    // A later deadline: on time again.
    m.set_deadline(t.id, Some(in_secs(600))).await.expect("set");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let v = m
            .deadlines()
            .await
            .expect("deadlines")
            .into_iter()
            .find(|v| v.id == t.id)
            .expect("listed");
        if v.standing == Standing::OnTime {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{v:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    m.set_deadline(t.id, None).await.expect("clear");
    assert!(
        m.deadlines()
            .await
            .expect("deadlines")
            .iter()
            .all(|v| v.id != t.id)
    );
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_links_lifetime_is_a_deadline() {
    let server = serve().await;
    let (m, dir) = open("link", 2).await;
    // A link that stops working a second after its first use: the site's links last ~1 s.
    let first = add(
        &m,
        format!("{server}/file/a.bin?size=8M&rate=256K&ttl=1"),
        None,
        true,
    )
    .await;
    wait(&m, first.id, "it starts", |t| t.received > 0).await;
    m.pause(first.id).await.expect("pause");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    m.start(first.id).await.expect("start");
    wait(&m, first.id, "the link expires", |t| {
        t.status == Status::Error
    })
    .await;
    let memory = m.site_memory(&server).await.expect("memory").expect("kept");
    assert!(memory.link_lifetime.is_some(), "{memory:?}");
    // Another download from the site, added paused: its deadline is its link's.
    let next = add(&m, format!("{server}/file/b.bin?size=1M"), None, false).await;
    let v = m
        .deadlines()
        .await
        .expect("deadlines")
        .into_iter()
        .find(|v| v.id == next.id)
        .expect("listed");
    assert_eq!(v.source, Source::Link);
    let lifetime = memory.link_lifetime.expect("lifetime") as i64 * 1000;
    assert!((v.at - (next.added_at + lifetime)).abs() < 2000, "{v:?}");
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}
