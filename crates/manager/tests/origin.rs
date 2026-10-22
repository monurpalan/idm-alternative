//! "Where did this file come from?" against the test server: a file traced by its path, by
//! its content after it was copied, by its mark, or not at all; its checksum checked again;
//! its address asked whether it changed.

use std::path::PathBuf;
use std::time::Duration;

use shardload_manager::origin::Traced;
use shardload_manager::{Config, Manager, NewTask, Status, Task, TaskId};
use shardload_testserver::router;

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
            .join("shardload-origin-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create workdir");
        Self(dir)
    }

    async fn manager(&self) -> Manager {
        Manager::open(
            &self.0.join("list.db"),
            Config {
                dir: self.0.join("downloads"),
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
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

async fn wait_done(m: &Manager, id: TaskId) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let t = m.get(id).await.expect("get");
        if matches!(t.status, Status::Done | Status::Error) {
            assert_eq!(t.status, Status::Done, "{:?}", t.error);
            return t;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn download(m: &Manager, url: String, checksum: Option<String>) -> Task {
    let t = m
        .add(NewTask {
            url,
            headers: vec![("Referer".into(), "https://example.com/downloads".into())],
            checksum,
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");
    wait_done(m, t.id).await
}

#[tokio::test(flavor = "multi_thread")]
async fn files_are_traced_by_path_content_or_mark() {
    let base = start_server().await;
    let w = WorkDir::new("trace");
    let m = w.manager().await;
    let url = format!("{base}/file/tool.zip?size={MB}&seed=51");
    let done = download(&m, url.clone(), None).await;
    let path = done.path.clone().expect("path");

    // Where it was saved.
    let o = m.file_origin(&path).await.expect("origin");
    assert_eq!(o.traced, Traced::Path);
    assert_eq!(o.url.as_deref(), Some(url.as_str()));
    assert_eq!(o.page.as_deref(), Some("https://example.com/downloads"));
    assert_eq!(o.downloaded_at, done.finished_at);

    // A copy somewhere else: the same content.
    let copy = w.0.join("elsewhere").join("renamed.zip");
    std::fs::create_dir_all(copy.parent().expect("parent")).expect("dir");
    std::fs::copy(&path, &copy).expect("copy");
    let o = m.file_origin(&copy).await.expect("origin");
    assert_eq!(o.traced, Traced::Content);
    assert_eq!(o.url.as_deref(), Some(url.as_str()));

    // Dropped on the window: only its size and content (or name) are known.
    let sum = shardload_engine::file_checksum(&copy, shardload_engine::Algorithm::Sha256)
        .await
        .expect("hash")
        .to_string();
    let hex = sum.trim_start_matches("sha256:");
    let found = m.find_by_content(MB, Some(hex), None).await.expect("find");
    assert_eq!(found.map(|h| h.url), Some(url.clone()));
    let found = m
        .find_by_content(MB, None, Some("Tool (1).zip"))
        .await
        .expect("find");
    assert!(found.is_some(), "by name and size");
    let zeros = "0".repeat(64);
    assert_eq!(
        m.find_by_content(MB, Some(&zeros), None)
            .await
            .expect("find"),
        None
    );

    // Same size, other bytes: not that download.
    let other = w.0.join("elsewhere").join("other.zip");
    std::fs::write(&other, vec![7u8; MB as usize]).expect("write");
    let o = m.file_origin(&other).await.expect("origin");
    assert_eq!(o.traced, Traced::Unknown);
    assert_eq!(o.url, None);

    // Not in the history, but marked by a browser.
    shardload_engine::mark_of_the_web(
        &other,
        "https://cdn.example.org/other.zip",
        &[("Referer".into(), "https://example.org/page".into())],
    );
    if shardload_engine::read_mark(&other).is_some() {
        let o = m.file_origin(&other).await.expect("origin");
        assert_eq!(o.traced, Traced::Mark);
        assert_eq!(o.url.as_deref(), Some("https://cdn.example.org/other.zip"));
        assert_eq!(o.page.as_deref(), Some("https://example.org/page"));
    }
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recorded_checksum_traces_a_copy_whose_original_is_gone_and_checks_it() {
    use sha2::{Digest, Sha256};
    let base = start_server().await;
    let w = WorkDir::new("checksum");
    let m = w.manager().await;
    let mut bytes = vec![0u8; MB as usize];
    shardload_testserver::content::fill(52, 0, &mut bytes);
    let sum: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let checksum = format!("sha256:{sum}");
    let done = download(
        &m,
        format!("{base}/file/iso.img?size={MB}&seed=52"),
        Some(checksum.clone()),
    )
    .await;
    let path = done.path.expect("path");
    let copy = w.0.join("copy.img");
    std::fs::copy(&path, &copy).expect("copy");
    std::fs::remove_file(&path).expect("remove");
    let o = m.file_origin(&copy).await.expect("origin");
    assert_eq!(o.traced, Traced::Content);
    assert_eq!(o.checksum.as_deref(), Some(checksum.as_str()));

    let check = m
        .check_file_checksum(&copy, o.checksum.as_deref(), o.url.as_deref())
        .await
        .expect("check");
    assert_eq!(check.matches, Some(true));
    assert_eq!(check.from.as_deref(), Some("download"));
    // One byte changed since.
    bytes[1000] ^= 1;
    std::fs::write(&copy, &bytes).expect("write");
    let check = m
        .check_file_checksum(&copy, o.checksum.as_deref(), None)
        .await
        .expect("check");
    assert_eq!(check.matches, Some(false));
    // Nothing to check against: says so.
    let check = m
        .check_file_checksum(&copy, None, None)
        .await
        .expect("check");
    assert_eq!(check.matches, None);
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_remote_file_is_noticed() {
    let base = start_server().await;
    let w = WorkDir::new("update");
    let m = w.manager().await;
    let url = format!("{base}/file/app.exe?size={MB}&seed=53");
    let done = download(&m, url.clone(), None).await;
    let at = done.finished_at;
    // The same file: same size, not modified since.
    let same = m.check_for_update(&url, MB, at).await.expect("check");
    assert_eq!(same.size, Some(MB));
    assert_eq!(same.changed, Some(false));
    assert!(same.etag.is_some() && same.last_modified.is_some());
    // The address serves another size now.
    let other = m.check_for_update(&url, MB + 1, at).await.expect("check");
    assert_eq!(other.changed, Some(true));
    assert_eq!(other.latest_release, None, "not a GitHub release");
    m.close().await;
}

#[tokio::test]
#[ignore = "network: GitHub's API"]
async fn a_github_release_asset_knows_the_newest_release() {
    let opts = shardload_engine::Options::default();
    let (release, latest) = shardload_engine::github_releases(
        "https://github.com/cli/cli/releases/download/v2.60.0/gh_2.60.0_windows_amd64.zip",
        &opts,
    )
    .await
    .expect("ask")
    .expect("a release asset");
    assert_eq!(release, "v2.60.0");
    assert!(latest.starts_with('v') && latest != release, "{latest}");
}
