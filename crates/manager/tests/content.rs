//! What a finished file really is, against the test server: a program named like a document,
//! a web page saved in place of the file, and names that say nothing fixed from the content.

use std::path::{Path, PathBuf};
use std::time::Duration;

use shardload_manager::{Config, ErrorKind, Manager, NewTask, Status, Task, TaskId};

async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, shardload_testserver::router()).await });
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

    fn path(&self) -> &Path {
        &self.0
    }

    fn config(&self) -> Config {
        Config {
            dir: self.0.join("downloads"),
            max_active: 4,
            connections: 2,
            engine: shardload_engine::Options {
                find_checksum: false,
                ..shardload_engine::Options::default()
            },
            ..Config::default()
        }
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

async fn finished(m: &Manager, id: TaskId) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let t = m.get(id).await.expect("get");
        if matches!(t.status, Status::Done | Status::Error) {
            return t;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out: {t:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn download(m: &Manager, url: String, name: Option<&str>, title: Option<&str>) -> Task {
    add(
        m,
        NewTask {
            url,
            name: name.map(str::to_owned),
            title: title.map(str::to_owned),
            ..NewTask::default()
        },
    )
    .await
}

async fn add(m: &Manager, new: NewTask) -> Task {
    let t = m.add(NewTask { start: true, ..new }).await.expect("add");
    finished(m, t.id).await
}

fn warning(t: &Task) -> Option<(&'static str, &'static str)> {
    t.warning.map(|w| (w.kind.as_str(), w.by))
}

#[tokio::test]
async fn content_against_name() {
    let base = start_server().await;
    let dir = WorkDir::new("content-against-name");
    let m = Manager::open(&dir.path().join("list.db"), dir.config())
        .await
        .expect("open");

    // A program named like a document: kept, with a warning from its content.
    let t = download(
        &m,
        format!("{base}/file/invoice.pdf?size=64K&kind=exe"),
        None,
        None,
    )
    .await;
    assert_eq!(t.status, Status::Done, "{:?}", t.error);
    assert_eq!(t.content.as_deref(), Some("program:exe"));
    assert_eq!(warning(&t), Some(("disguised", "content")));
    assert_eq!(
        t.name.as_deref(),
        Some("invoice.pdf"),
        "a warning, not a rename"
    );

    // The server's Content-Disposition names it a photo; its bytes are a program.
    let t = download(
        &m,
        format!("{base}/file/get?size=32K&kind=exe&cd=holiday.jpg"),
        None,
        None,
    )
    .await;
    assert_eq!(t.name.as_deref(), Some("holiday.jpg"));
    assert_eq!(warning(&t), Some(("disguised", "content")));

    // Its name alone gives it away.
    let t = download(
        &m,
        format!("{base}/file/report.pdf.exe?size=32K&kind=exe"),
        None,
        None,
    )
    .await;
    assert_eq!(warning(&t), Some(("disguised", "name")));

    // Formats built on ZIP are not programs, and a program named as one is fine.
    for (url, content) in [
        ("report.docx?size=32K&kind=docx", "document:docx"),
        ("app.apk?size=32K&kind=zip", "archive:zip"),
        ("setup.exe?size=32K&kind=exe", "program:exe"),
        ("notes.txt?size=32K&kind=sh", "script:sh"),
    ] {
        let t = download(&m, format!("{base}/file/{url}"), None, None).await;
        assert_eq!(t.status, Status::Done, "{url}");
        assert_eq!(t.content.as_deref(), Some(content), "{url}");
        assert_eq!(warning(&t), None, "{url}");
    }
    // Bytes that are nothing in particular.
    let t = download(&m, format!("{base}/file/blob.dat?size=32K"), None, None).await;
    assert_eq!(t.content, None);
    assert_eq!(warning(&t), None);
    m.close().await;
}

#[tokio::test]
async fn web_page_instead_of_the_file() {
    let base = start_server().await;
    let dir = WorkDir::new("content-web-page");
    let m = Manager::open(&dir.path().join("list.db"), dir.config())
        .await
        .expect("open");

    // Sent as a file (octet-stream), but it is a sign-in page.
    let t = download(
        &m,
        format!("{base}/file/data.zip?size=8K&kind=html"),
        None,
        None,
    )
    .await;
    assert_eq!(t.status, Status::Error);
    assert_eq!(t.error_kind, Some(ErrorKind::Expired));
    assert!(
        t.error.as_deref().is_some_and(|e| e.contains("web page")),
        "{:?}",
        t.error
    );
    assert_eq!(t.received, 0);
    let path = t.path.clone().expect("path");
    assert!(!path.exists(), "the page is not kept as data.zip");
    assert!(m.history().await.expect("history").is_empty());

    // A page the user asked for, or a name that promises nothing, is just a page.
    let t = download(
        &m,
        format!("{base}/file/page.html?size=8K&kind=html"),
        None,
        None,
    )
    .await;
    assert_eq!(t.status, Status::Done);
    let t = download(
        &m,
        format!("{base}/file/view.php?size=8K&kind=html"),
        None,
        None,
    )
    .await;
    assert_eq!(t.status, Status::Done);
    assert_eq!(t.name.as_deref(), Some("view.html"), "named by what it is");
    m.close().await;
}

#[tokio::test]
async fn names_that_say_nothing() {
    let base = start_server().await;
    let dir = WorkDir::new("content-names");
    let m = Manager::open(&dir.path().join("list.db"), dir.config())
        .await
        .expect("open");

    let t = download(
        &m,
        format!("{base}/file/download.php?size=32K&kind=pdf"),
        None,
        None,
    )
    .await;
    assert_eq!(t.name.as_deref(), Some("download.pdf"));
    let path = t.path.clone().expect("path");
    assert!(path.is_file() && path.ends_with("download.pdf"), "{path:?}");
    assert!(!path.with_file_name("download.php").exists());
    assert_eq!(
        t.category,
        shardload_manager::Category::Documents,
        "the category follows"
    );
    // Recorded just after the download is marked done.
    let mut history = m.history().await.expect("history");
    for _ in 0..40 {
        if !history.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        history = m.history().await.expect("history");
    }
    assert_eq!(history[0].name, "download.pdf");

    // The page's title when the stem says nothing either; a second one gets a number.
    let url = format!("{base}/file/get.php?size=32K&kind=pdf&seed=2");
    let t = download(&m, url.clone(), None, Some("Annual report 2025")).await;
    assert_eq!(t.name.as_deref(), Some("Annual report 2025.pdf"));
    let t = download(&m, format!("{url}&n=2"), None, Some("Annual report 2025")).await;
    assert_eq!(t.name.as_deref(), Some("Annual report 2025 (1).pdf"));

    // No extension at all.
    let t = download(
        &m,
        format!("{base}/file/12345?size=32K&kind=mp4"),
        None,
        None,
    )
    .await;
    assert_eq!(t.name.as_deref(), Some("12345.mp4"));
    assert_eq!(t.category, shardload_manager::Category::Video);

    // A name the user gave is never changed.
    let t = download(
        &m,
        format!("{base}/file/download.php?size=32K&kind=pdf&seed=3"),
        Some("mine.php"),
        Some("A title"),
    )
    .await;
    assert_eq!(t.name.as_deref(), Some("mine.php"));
    assert_eq!(t.content.as_deref(), Some("document:pdf"));
    m.close().await;
}

/// SHA-256 of the test server's bytes for `seed` and `size`, as `sha256:<hex>`.
fn sha256_of(seed: u64, size: usize) -> String {
    use sha2::{Digest, Sha256};
    let mut bytes = vec![0u8; size];
    shardload_testserver::content::fill(seed, 0, &mut bytes);
    let hex: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

#[tokio::test]
async fn a_checksum_shown_on_the_page() {
    let base = start_server().await;
    let dir = WorkDir::new("content-page-checksum");
    let m = Manager::open(&dir.path().join("list.db"), dir.config())
        .await
        .expect("open");
    let size = 256 * 1024;
    let page = |url: String, checksum: String| NewTask {
        url,
        page_checksum: Some(checksum),
        ..NewTask::default()
    };

    let t = add(
        &m,
        page(
            format!("{base}/file/a.bin?size=256K&seed=41"),
            sha256_of(41, size),
        ),
    )
    .await;
    assert_eq!(t.status, Status::Done, "{:?}", t.error);
    assert_eq!(t.checksum, Some(sha256_of(41, size)));
    assert_eq!(
        t.checksum_from.as_deref(),
        Some(shardload_engine::FROM_PAGE)
    );

    // The page shows another file's: said as the page's, not as a corrupt file.
    let t = add(
        &m,
        page(
            format!("{base}/file/b.bin?size=256K&seed=42"),
            sha256_of(43, size),
        ),
    )
    .await;
    assert_eq!(t.status, Status::Error);
    assert_eq!(t.error_kind, Some(ErrorKind::Checksum));
    assert_eq!(
        t.checksum_from.as_deref(),
        Some(shardload_engine::FROM_PAGE)
    );
    assert!(
        t.error
            .as_deref()
            .is_some_and(|e| e.contains("shown on the page")),
        "{:?}",
        t.error
    );

    // One that does not parse is left out; the download goes on without it.
    let t = add(
        &m,
        page(
            format!("{base}/file/c.bin?size=256K&seed=44"),
            "sha256:nothex".into(),
        ),
    )
    .await;
    assert_eq!(t.status, Status::Done);
    assert_eq!(t.checksum, None);

    // A checksum the user gave wins.
    let t = add(
        &m,
        NewTask {
            checksum: Some(sha256_of(45, size)),
            ..page(
                format!("{base}/file/d.bin?size=256K&seed=45"),
                sha256_of(46, size),
            )
        },
    )
    .await;
    assert_eq!(t.status, Status::Done, "{:?}", t.error);
    assert_eq!(t.checksum_from, None);
    m.close().await;
}
