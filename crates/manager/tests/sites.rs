//! Site memory against the test server: a second download from a site starts with what the
//! first one found out.

use std::path::PathBuf;
use std::time::Duration;

use shardload_manager::{Config, ErrorKind, Manager, NewTask, Status, Task, TaskId};
use shardload_testserver::router;

const MB: u64 = 1 << 20;

async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(
            listener,
            router().into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
    format!("http://{addr}")
}

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir()
            .join("shardload-site-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create workdir");
        Self(dir)
    }

    async fn manager(&self) -> Manager {
        let config = Config {
            dir: self.0.join("downloads"),
            connections: 16,
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

async fn wait_end(m: &Manager, id: TaskId) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let t = m.get(id).await.expect("get");
        if matches!(t.status, Status::Done | Status::Error) {
            return t;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn download(m: &Manager, url: String) -> Task {
    let t = m
        .add(NewTask {
            url,
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");
    let t = wait_end(m, t.id).await;
    assert_eq!(t.status, Status::Done, "{:?}", t.error);
    t
}

/// The test server's counters for the URL whose path and query contain `part`.
async fn stats(base: &str, part: &str) -> serde_json::Value {
    let all: Vec<serde_json::Value> = reqwest::get(format!("{base}/_stats"))
        .await
        .expect("stats")
        .json()
        .await
        .expect("json");
    all.into_iter()
        .find(|u| u["url"].as_str().is_some_and(|s| s.contains(part)))
        .expect("url in stats")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_limit_is_remembered_for_the_next_download() {
    let base = start_server().await;
    let w = WorkDir::new("limit");
    let m = w.manager().await;
    let q = format!("size={}&rate=2M&max_conns=4&refuse_status=403", 16 * MB);
    download(&m, format!("{base}/file/a.bin?{q}&seed=1")).await;
    let first = stats(&base, "seed=1").await;
    let sites = m.sites().await.expect("sites");
    assert_eq!(sites.len(), 1, "{sites:?}");
    let max = sites[0].max_connections.expect("learned a limit");
    assert!((1..=4).contains(&max), "{max}");

    // The next download from the site starts at one more than learned: at most one refusal
    // instead of a dozen.
    download(&m, format!("{base}/file/b.bin?{q}&seed=2")).await;
    let second = stats(&base, "seed=2").await;
    assert!(
        first["refused"].as_u64() > Some(2),
        "first refused {}",
        first["refused"]
    );
    assert!(
        second["refused"].as_u64() <= Some(1),
        "second refused {}",
        second["refused"]
    );
    assert!(second["peak_active"].as_u64() <= Some(max as u64 + 1));
    let max = m.sites().await.expect("sites")[0].max_connections;
    assert!(max.is_some_and(|m| m <= 4), "{max:?}");

    m.forget_site(&sites[0].site).await.expect("forget");
    assert!(m.sites().await.expect("sites").is_empty());
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_without_ranges_gets_one_connection_from_the_start() {
    let base = start_server().await;
    let w = WorkDir::new("ranges");
    let m = w.manager().await;
    let q = format!("size={}&range_as_200=1", 4 * MB);
    download(&m, format!("{base}/file/a.bin?{q}&seed=1")).await;
    let memory = m
        .site_memory(&base)
        .await
        .expect("memory")
        .expect("remembered");
    assert!(memory.no_ranges);
    download(&m, format!("{base}/file/b.bin?{q}&seed=2")).await;
    // The manager's question about the name, then one request for the whole file.
    assert_eq!(stats(&base, "seed=2").await["gets"], 2);
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_identity_a_site_serves_is_found_and_kept() {
    let base = start_server().await;
    let w = WorkDir::new("identity");
    let m = w.manager().await;
    // Serves Firefox only; the settings say Chrome.
    let q = format!("size={}&only_ua=Firefox", MB);
    let t = download(&m, format!("{base}/file/a.bin?{q}&seed=1")).await;
    assert_eq!(t.size, Some(MB));
    let memory = m.site_memory(&base).await.expect("memory").expect("kept");
    assert_eq!(memory.browser.as_deref(), Some("firefox"));
    let ua = stats(&base, "seed=1").await["user_agent"].clone();
    assert!(ua.as_str().is_some_and(|u| u.contains("Firefox")), "{ua}");

    // The next one goes as Firefox straight away.
    download(&m, format!("{base}/file/b.bin?{q}&seed=2")).await;
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn how_long_links_last_is_estimated() {
    let base = start_server().await;
    let w = WorkDir::new("lifetime");
    let m = w.manager().await;
    let t = m
        .add(NewTask {
            url: format!("{base}/file/a.bin?size={}&rate=256K&ttl=1", 8 * MB),
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while m.get(t.id).await.expect("get").received == 0 {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    m.pause(t.id).await.expect("pause");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    m.start(t.id).await.expect("start");
    let failed = wait_end(&m, t.id).await;
    assert_eq!(
        failed.error_kind,
        Some(ErrorKind::Expired),
        "{:?}",
        failed.error
    );
    let memory = m.site_memory(&base).await.expect("memory").expect("kept");
    assert!(memory.link_lifetime.is_some_and(|s| s <= 10), "{memory:?}");
    m.close().await;
}
