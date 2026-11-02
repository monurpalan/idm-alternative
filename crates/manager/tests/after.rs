//! What happens when a download finishes, against the test server: unpacking, moving by a
//! rule, the user's command and webhook, and what goes wrong with each.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shardload_manager::after::{After, Done, Limits, Rule};
use shardload_manager::{Config, Manager, NewTask, Status, Task, TaskId};

async fn serve(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, router).await });
    format!("http://{addr}")
}

/// A webhook receiver: keeps every body it gets; waits `delay` before answering.
async fn hooks(delay: Duration) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&got);
    let router = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |body: axum::Json<serde_json::Value>| {
            let kept = Arc::clone(&kept);
            async move {
                tokio::time::sleep(delay).await;
                kept.lock().expect("lock").push(body.0);
                "ok"
            }
        }),
    );
    (format!("{}/hook", serve(router).await), got)
}

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("shardload-manager-tests")
        .join(format!("after-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("workdir");
    dir
}

async fn open(dir: &Path, rules: Vec<Rule>, limits: Limits) -> Manager {
    Manager::open(
        &dir.join("list.db"),
        Config {
            dir: dir.join("downloads"),
            connections: 4,
            engine: shardload_engine::Options {
                find_checksum: false,
                ..shardload_engine::Options::default()
            },
            rules,
            after_limits: limits,
            ..Config::default()
        },
    )
    .await
    .expect("open")
}

/// Downloads `url` and waits until its actions have run (their log is written).
async fn finish(m: &Manager, url: String, name: &str, after: Option<After>) -> Task {
    let t = m
        .add(NewTask {
            url,
            name: Some(name.into()),
            start: true,
            after,
            ..NewTask::default()
        })
        .await
        .expect("add");
    wait(m, t.id, |t| {
        t.status == Status::Done && !t.after_log.is_empty()
    })
    .await
}

async fn wait(m: &Manager, id: TaskId, pred: impl Fn(&Task) -> bool) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let t = m.get(id).await.expect("get");
        if pred(&t) {
            return t;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out: {t:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn find<'a>(log: &'a [Done], action: &str) -> &'a Done {
    log.iter()
        .find(|d| d.action == action)
        .unwrap_or_else(|| panic!("no {action} in {log:?}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn unpacks_when_the_download_asks() {
    let server = serve(shardload_testserver::router()).await;
    let dir = workdir("unpack");
    let m = open(&dir, Vec::new(), Limits::default()).await;
    let t = finish(
        &m,
        format!("{server}/zip/sample.zip"),
        "sample.zip",
        Some(After {
            unpack: true,
            move_to: None,
        }),
    )
    .await;
    let unpack = find(&t.after_log, "unpack");
    // The sample holds an encrypted, an LZMA and a hostile entry: left out and said.
    assert!(!unpack.ok && unpack.left_out == 3, "{unpack:?}");
    assert_eq!(unpack.count, Some(4), "{unpack:?}");
    let out = dir.join("downloads").join("sample");
    assert_eq!(
        std::fs::read_to_string(out.join("readme.txt")).expect("readme"),
        "sample archive 1\n"
    );
    assert!(out.join("data/big.bin").is_file());
    assert!(!dir.join("downloads/evil.txt").exists());
    assert!(t.path.expect("path").is_file(), "the archive stays");
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rule_moves_runs_and_tells() {
    let server = serve(shardload_testserver::router()).await;
    let (hook, got) = hooks(Duration::ZERO).await;
    let dir = workdir("rule");
    let moved = dir.join("isos");
    #[cfg(windows)]
    let command = "fc.exe /B {file} {file}";
    #[cfg(not(windows))]
    let command = "test -f {file}";
    let rules = vec![
        Rule {
            name: Some("*.iso".into()),
            move_to: Some(moved.clone()),
            command: Some(command.into()),
            webhook: Some(hook),
            ..Rule::default()
        },
        // Not for this download: never runs.
        Rule {
            name: Some("*.zip".into()),
            command: Some("no-such-program".into()),
            ..Rule::default()
        },
    ];
    let m = open(&dir, rules, Limits::default()).await;
    let t = finish(&m, format!("{server}/file/a.iso?size=2M"), "disk.iso", None).await;
    let path = t.path.clone().expect("path");
    assert_eq!(path, moved.join("disk.iso"));
    assert!(path.is_file());
    assert!(find(&t.after_log, "move").ok, "{:?}", t.after_log);
    assert!(find(&t.after_log, "command").ok, "{:?}", t.after_log);
    assert!(find(&t.after_log, "webhook").ok, "{:?}", t.after_log);
    assert_eq!(find(&t.after_log, "webhook").count, Some(200));
    assert_eq!(find(&t.after_log, "command").count, Some(0));
    assert_eq!(t.after_log.len(), 3, "{:?}", t.after_log);
    let bodies = got.lock().expect("lock").clone();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["event"], "download.done");
    assert_eq!(bodies[0]["name"], "disk.iso");
    assert_eq!(bodies[0]["path"], path.to_string_lossy().as_ref());
    assert_eq!(bodies[0]["size"], 2 << 20);
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn failures_are_kept_and_the_download_stays_done() {
    let server = serve(shardload_testserver::router()).await;
    let (hook, got) = hooks(Duration::from_secs(5)).await;
    let dir = workdir("fail");
    let rules = vec![Rule {
        unpack: true,
        command: Some("no-such-program-anywhere {file}".into()),
        webhook: Some(hook),
        ..Rule::default()
    }];
    let limits = Limits {
        webhook: Duration::from_millis(300),
        ..Limits::default()
    };
    let m = open(&dir, rules, limits).await;
    // Named like an archive, but not one.
    let t = finish(
        &m,
        format!("{server}/file/x.zip?size=64K"),
        "broken.zip",
        None,
    )
    .await;
    assert_eq!(t.status, Status::Done);
    let unpack = find(&t.after_log, "unpack");
    assert!(
        !unpack.ok && unpack.detail.contains("not a ZIP"),
        "{unpack:?}"
    );
    let command = find(&t.after_log, "command");
    assert!(
        !command.ok && command.detail.contains("could not run"),
        "{command:?}"
    );
    let webhook = find(&t.after_log, "webhook");
    assert!(!webhook.ok, "{webhook:?}");
    assert!(
        got.lock().expect("lock").is_empty(),
        "answered too late to count"
    );
    assert!(!dir.join("downloads").join("broken").exists());
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_happens_without_a_wish_or_a_rule() {
    let server = serve(shardload_testserver::router()).await;
    let dir = workdir("none");
    let m = open(&dir, Vec::new(), Limits::default()).await;
    let t = m
        .add(NewTask {
            url: format!("{server}/zip/sample.zip"),
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");
    let t = wait(&m, t.id, |t| t.status == Status::Done).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let t = m.get(t.id).await.expect("get");
    assert!(t.after_log.is_empty(), "{:?}", t.after_log);
    assert!(!dir.join("downloads").join("sample").exists());
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}
