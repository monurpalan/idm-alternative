//! Continuing downloads the browser left interrupted, against the test server: the partial
//! file is taken over when the server serves the same file, and the download starts over
//! when it does not.

use std::path::{Path, PathBuf};
use std::time::Duration;

use shardload_manager::rescue::Rescue;
use shardload_manager::{Config, Manager, Status, Task, TaskId};
use shardload_testserver::{Verdict, verify_file};

const MB: u64 = 1 << 20;
const SIZE: u64 = 10 * MB;

async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, shardload_testserver::router()).await });
    format!("http://{addr}")
}

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("shardload-manager-tests")
        .join(format!("rescue-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("browser")).expect("workdir");
    dir
}

async fn open(dir: &Path) -> Manager {
    Manager::open(
        &dir.join("list.db"),
        Config {
            dir: dir.join("downloads"),
            // One connection: what the server sends is then what the download needs, without
            // what parallel connections leave in their buffers when their ranges move.
            connections: 1,
            engine: shardload_engine::Options {
                find_checksum: false,
                ..shardload_engine::Options::default()
            },
            ..Config::default()
        },
    )
    .await
    .expect("open")
}

/// The first `len` bytes of the test file for `seed`, as the browser left them.
fn partial(path: &Path, seed: u64, len: u64) {
    let mut buf = vec![0u8; len as usize];
    shardload_testserver::content::fill(seed, 0, &mut buf);
    std::fs::write(path, buf).expect("partial");
}

async fn done(m: &Manager, id: TaskId) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let t = m.get(id).await.expect("get");
        if matches!(t.status, Status::Done | Status::Error) {
            return t;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out: {t:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Bytes the server sent for URLs containing `name`.
async fn sent(server: &str, name: &str) -> u64 {
    let stats: serde_json::Value = reqwest::get(format!("{server}/_stats"))
        .await
        .expect("stats")
        .json()
        .await
        .expect("json");
    stats
        .as_array()
        .expect("array")
        .iter()
        .filter(|s| s["url"].as_str().is_some_and(|u| u.contains(name)))
        .map(|s| s["bytes_sent"].as_u64().unwrap_or(0))
        .sum()
}

fn rescue(url: String, target: PathBuf, received: u64, size: Option<u64>) -> Rescue {
    Rescue {
        url,
        target,
        received,
        size,
        ..Rescue::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn takes_over_chromiums_partial_file() {
    let server = serve().await;
    let dir = workdir("chromium");
    let target = dir.join("browser").join("big file.bin");
    partial(&dir.join("browser/big file.bin.crdownload"), 1, 6 * MB);
    let m = open(&dir).await;
    let url = format!("{server}/file/kept.bin?size=10M");
    // The browser counted a little more than it wrote: only what is on disk counts.
    let r = m
        .rescue(rescue(url, target.clone(), 6 * MB + 5000, Some(SIZE)))
        .await
        .expect("rescue");
    assert_eq!(r.kept, 6 * MB, "{:?}", r.why);
    let t = done(&m, r.task.id).await;
    assert_eq!(t.status, Status::Done, "{t:?}");
    assert_eq!(t.path.as_deref(), Some(target.as_path()));
    assert_eq!(verify_file(&target, 1, SIZE).expect("verify"), Verdict::Ok);
    assert!(!dir.join("browser/big file.bin.crdownload").exists());
    // The rest, the last megabyte again (it may not have reached the disk) and the samples.
    let bytes = sent(&server, "kept.bin").await;
    assert!(bytes < 5 * MB + MB / 2, "sent {bytes}");
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn takes_over_firefoxs_partial_file() {
    let server = serve().await;
    let dir = workdir("firefox");
    let target = dir.join("browser").join("f.bin");
    partial(&dir.join("browser/f.bin.part"), 1, 3 * MB);
    let m = open(&dir).await;
    let url = format!("{server}/file/ff.bin?size=10M");
    let r = m
        .rescue(rescue(url, target.clone(), 3 * MB, None))
        .await
        .expect("rescue");
    assert_eq!(r.kept, 3 * MB, "{:?}", r.why);
    assert_eq!(done(&m, r.task.id).await.status, Status::Done);
    assert_eq!(verify_file(&target, 1, SIZE).expect("verify"), Verdict::Ok);
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn starts_over_when_the_file_changed() {
    let server = serve().await;
    let dir = workdir("changed");
    let m = open(&dir).await;
    // Other bytes (another version of the file) under the same name.
    let target = dir.join("browser/a.bin");
    partial(&dir.join("browser/a.bin.crdownload"), 2, 4 * MB);
    let r = m
        .rescue(rescue(
            format!("{server}/file/a.bin?size=10M"),
            target.clone(),
            4 * MB,
            Some(SIZE),
        ))
        .await
        .expect("rescue");
    assert_eq!(r.kept, 0);
    assert!(
        r.why.as_deref().is_some_and(|w| w.contains("different")),
        "{:?}",
        r.why
    );
    assert!(
        !dir.join("browser/a.bin.crdownload").exists(),
        "the stale part goes"
    );
    assert_eq!(done(&m, r.task.id).await.status, Status::Done);
    assert_eq!(verify_file(&target, 1, SIZE).expect("verify"), Verdict::Ok);
    // Another size than the browser was told.
    let target = dir.join("browser/b.bin");
    partial(&dir.join("browser/b.bin.crdownload"), 1, 4 * MB);
    let r = m
        .rescue(rescue(
            format!("{server}/file/b.bin?size=10M"),
            target.clone(),
            4 * MB,
            Some(12 * MB),
        ))
        .await
        .expect("rescue");
    assert_eq!(r.kept, 0);
    assert!(
        r.why.as_deref().is_some_and(|w| w.contains("size")),
        "{:?}",
        r.why
    );
    assert_eq!(done(&m, r.task.id).await.status, Status::Done);
    assert_eq!(verify_file(&target, 1, SIZE).expect("verify"), Verdict::Ok);
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_etag_on_every_answer_starts_over() {
    let server = serve().await;
    let dir = workdir("etag");
    let m = open(&dir).await;
    let target = dir.join("browser/e.bin");
    partial(&dir.join("browser/e.bin.crdownload"), 1, 4 * MB);
    let url = format!("{server}/file/e.bin?size=10M&etag=rotate");
    let r = m
        .rescue(rescue(url, target.clone(), 4 * MB, Some(SIZE)))
        .await
        .expect("rescue");
    // The same bytes, so the part is taken; but when the download resumes, the server says
    // the file is another one (a new ETag), so it starts from the beginning.
    assert_eq!(done(&m, r.task.id).await.status, Status::Done);
    assert_eq!(verify_file(&target, 1, SIZE).expect("verify"), Verdict::Ok);
    let bytes = sent(&server, "e.bin").await;
    assert!(bytes >= SIZE, "sent {bytes}: started over");
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_partial_file_it_goes_to_the_download_folder() {
    let server = serve().await;
    let dir = workdir("none");
    let m = open(&dir).await;
    // A folder the browser never wrote to here: not used.
    let target = dir.join("elsewhere/x.bin");
    let r = m
        .rescue(rescue(
            format!("{server}/file/x.bin?size=2M"),
            target.clone(),
            MB,
            None,
        ))
        .await
        .expect("rescue");
    assert_eq!(r.kept, 0);
    let t = done(&m, r.task.id).await;
    assert_eq!(t.path, Some(dir.join("downloads/x.bin")));
    assert!(!dir.join("elsewhere").exists());
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}
