//! Files picked out of a ZIP archive on the server, as downloads in the list.

use std::path::PathBuf;
use std::time::Duration;

use shardload_manager::{ArchiveChoice, Config, Manager, NewTask, Status, Stream, Task, TaskId};
use shardload_testserver::router;
use shardload_testserver::zip::{sample, sample_entries};

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
            .join("shardload-manager-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create workdir");
        Self(dir)
    }

    fn config(&self) -> Config {
        Config {
            dir: self.0.join("downloads"),
            ..Config::default()
        }
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
            return t;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out: {t:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn archive(url: String, entries: &[&str]) -> NewTask {
    NewTask {
        url,
        start: true,
        stream: Some(Stream {
            archive: Some(ArchiveChoice {
                entries: entries.iter().map(|e| (*e).to_owned()).collect(),
                ..ArchiveChoice::default()
            }),
            ..Stream::default()
        }),
        ..NewTask::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn picks_files_into_a_folder_named_after_the_archive() {
    let base = start_server().await;
    let w = WorkDir::new("archive");
    let m = Manager::open(&w.0.join("list.db"), w.config())
        .await
        .expect("open");
    let url = format!("{base}/zip/sample.zip?seed=11");

    let info = m.inspect_archive(&url, &[]).await.expect("inspect");
    assert_eq!(info.name, "sample.zip");
    assert_eq!(info.entries.len(), sample_entries(11).len());
    let secret = info
        .entries
        .iter()
        .find(|e| e.name == "secret.txt")
        .expect("secret");
    assert!(secret.encrypted && secret.unsupported.is_some());

    let t = m
        .add(archive(url.clone(), &["readme.txt", "data/raw.bin"]))
        .await
        .expect("add");
    let t = wait_done(&m, t.id).await;
    assert_eq!(t.status, Status::Done, "{:?}", t.error);
    let folder = w.0.join("downloads").join("sample");
    assert_eq!(t.path.as_deref(), Some(folder.as_path()));
    for name in ["readme.txt", "data/raw.bin"] {
        let want = sample_entries(11)
            .into_iter()
            .find(|e| e.name == name)
            .expect("entry");
        let got = std::fs::read(folder.join(name)).expect("read");
        assert!(got == want.data, "{name}");
    }
    assert!(!folder.join("docs").exists());

    // An entry that cannot be extracted fails the download, with the reason.
    let t = m.add(archive(url, &["secret.txt"])).await.expect("add");
    let t = wait_done(&m, t.id).await;
    assert_eq!(t.status, Status::Error);
    assert!(t.error.unwrap_or_default().contains("encrypted"));
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_ranges_the_whole_archive_is_downloaded() {
    let base = start_server().await;
    let w = WorkDir::new("archive-whole");
    let m = Manager::open(&w.0.join("list.db"), w.config())
        .await
        .expect("open");
    let url = format!("{base}/zip/sample.zip?seed=12&ranges=0");
    assert!(m.inspect_archive(&url, &[]).await.is_err());
    let t = m.add(archive(url, &["readme.txt"])).await.expect("add");
    let t = wait_done(&m, t.id).await;
    assert_eq!(t.status, Status::Done, "{:?}", t.error);
    let path = t.path.expect("path");
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some("sample.zip")
    );
    assert!(std::fs::read(&path).expect("read") == sample(12, false));
    let whole = t.stream.and_then(|s| s.archive).expect("archive");
    assert!(whole.whole);
    m.close().await;
}
