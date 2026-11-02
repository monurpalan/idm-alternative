//! "Why slow?" hints against the test server: each shows for its cause, and a plain download
//! shows none.

use std::time::{Duration, Instant};

use shardload_manager::hints::Hint;
use shardload_manager::{Config, Event, Manager, NewTask, Status};

struct Run {
    /// Every hint seen in a progress event, in order of first appearance.
    hints: Vec<Hint>,
    status: Status,
}

/// Downloads `path` from a fresh test server until it ends or `until` is seen.
async fn run(
    name: &str,
    path: &str,
    connections: usize,
    speed_limit: Option<u64>,
    engine: shardload_engine::Options,
    until: Option<fn(&Hint) -> bool>,
) -> Run {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, shardload_testserver::router()).await });
    let dir = std::env::temp_dir()
        .join("shardload-manager-tests")
        .join(format!("hints-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("workdir");
    let m = Manager::open(
        &dir.join("list.db"),
        Config {
            dir: dir.join("downloads"),
            connections,
            engine: shardload_engine::Options {
                find_checksum: false,
                ..engine
            },
            ..Config::default()
        },
    )
    .await
    .expect("open");
    let mut events = m.subscribe();
    let t = m
        .add(NewTask {
            url: format!("http://{addr}{path}"),
            start: true,
            speed_limit,
            ..NewTask::default()
        })
        .await
        .expect("add");
    let mut hints: Vec<Hint> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        if let Ok(Ok(Event::Progress(p))) =
            tokio::time::timeout(Duration::from_millis(500), events.recv()).await
            && p.id == t.id
        {
            for h in p.hints {
                if !hints.contains(&h) {
                    hints.push(h);
                }
            }
        }
        let status = m.get(t.id).await.expect("get").status;
        let found = until.is_some_and(|f| hints.iter().any(f));
        if found || matches!(status, Status::Done | Status::Error) {
            break status;
        }
        assert!(Instant::now() < deadline, "too slow; hints {hints:?}");
    };
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
    Run { hints, status }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plain_download_shows_nothing() {
    // Fast enough to take a few seconds at most, slow enough for many progress events.
    let r = run(
        "plain",
        "/file/plain.bin?size=40M&rate=4M",
        4,
        None,
        shardload_engine::Options::default(),
        None,
    )
    .await;
    assert_eq!(r.status, Status::Done);
    assert!(r.hints.is_empty(), "{:?}", r.hints);
}

#[tokio::test(flavor = "multi_thread")]
async fn its_own_speed_limit() {
    let r = run(
        "limit",
        "/file/limit.bin?size=20M",
        4,
        Some(1 << 20),
        shardload_engine::Options::default(),
        Some(|h| matches!(h, Hint::Limit { .. })),
    )
    .await;
    assert_eq!(
        r.hints,
        vec![Hint::Limit {
            rate: 1 << 20,
            own: true
        }]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disk_that_cannot_keep_up() {
    // Every write of up to 1 MiB waits a second: the connections spend most of their time on it.
    let r = run(
        "disk",
        "/file/disk.bin?size=200M",
        4,
        None,
        shardload_engine::Options {
            slow_disk: Some(Duration::from_secs(1)),
            ..shardload_engine::Options::default()
        },
        Some(|h| *h == Hint::Disk),
    )
    .await;
    assert_eq!(r.hints, vec![Hint::Disk]);
}

#[tokio::test(flavor = "multi_thread")]
async fn connections_that_keep_dropping() {
    let r = run(
        "drops",
        "/file/drops.bin?size=40M&rate=2M&drop_after=512K",
        4,
        None,
        shardload_engine::Options {
            max_failures: 1000,
            ..shardload_engine::Options::default()
        },
        Some(|h| matches!(h, Hint::Drops { .. })),
    )
    .await;
    assert!(
        matches!(r.hints.as_slice(), [Hint::Drops { count }] if *count >= 3),
        "{:?}",
        r.hints
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_allows_few_connections() {
    let r = run(
        "server",
        "/file/server.bin?size=40M&rate=2M&max_conns=2",
        8,
        None,
        shardload_engine::Options::default(),
        Some(|h| matches!(h, Hint::Server { .. })),
    )
    .await;
    assert!(
        matches!(r.hints.as_slice(), [Hint::Server { connections }] if *connections <= 2),
        "{:?}",
        r.hints
    );
}
