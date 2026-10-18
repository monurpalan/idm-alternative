//! Torrents against a seed on this machine: a second librqbit session serves a folder over
//! 127.0.0.1, and the manager is told to try it as a peer (no DHT, no trackers, no
//! internet).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use idma_manager::{
    Config, Manager, NewTask, Status, Stream, Task, TaskId, TorrentChoice, TorrentConfig,
};
use librqbit::{CreateTorrentOptions, ListenerOptions, Session, SessionOptions};

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir()
            .join("idma-torrent-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create workdir");
        Self(dir)
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Deterministic, not repeating within a piece.
fn content(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

struct Seed {
    session: Arc<Session>,
    addr: SocketAddr,
    /// The `.torrent` file.
    torrent: PathBuf,
}

/// Serves `path` (a file or a folder) over loopback.
async fn seed(path: &Path, torrent_file: &Path) -> Seed {
    let session = Session::new_with_opts(
        path.parent().expect("parent").to_owned(),
        SessionOptions {
            dht: None,
            disable_trackers: true,
            persistence: None,
            disable_local_service_discovery: true,
            listen: Some(ListenerOptions {
                listen_addr: "127.0.0.1:0".parse().expect("addr"),
                ..ListenerOptions::default()
            }),
            ..SessionOptions::default()
        },
    )
    .await
    .expect("seed session");
    let (created, _handle) = session
        .create_and_serve_torrent(
            path,
            CreateTorrentOptions {
                name: None,
                trackers: Vec::new(),
                piece_length: Some(64 << 10),
            },
        )
        .await
        .expect("serve");
    std::fs::write(torrent_file, created.as_bytes().expect("bytes")).expect("write torrent");
    let addr = session.listen_addr().expect("listening");
    Seed {
        session,
        addr,
        torrent: torrent_file.to_owned(),
    }
}

async fn manager(dir: &Path, seed: SocketAddr) -> Manager {
    Manager::open(
        &dir.join("list.db"),
        Config {
            dir: dir.join("downloads"),
            torrent: TorrentConfig {
                dht: false,
                trackers: false,
                peers: vec![seed],
            },
            ..Config::default()
        },
    )
    .await
    .expect("open")
}

async fn wait_for(m: &Manager, id: TaskId, want: Status) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let t = m.get(id).await.expect("get");
        if t.status == want {
            return t;
        }
        assert!(
            t.status != Status::Error || want == Status::Error,
            "failed: {:?}",
            t.error
        );
        assert!(tokio::time::Instant::now() < deadline, "stuck at {t:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_a_single_file_torrent() {
    let w = WorkDir::new("single");
    let source = w.0.join("source");
    std::fs::create_dir_all(&source).expect("mkdir");
    let data = content(1, 3 * (1 << 20) + 12_345);
    std::fs::write(source.join("film.mkv"), &data).expect("write");
    let s = seed(&source.join("film.mkv"), &w.0.join("film.torrent")).await;

    let m = manager(&w.0, s.addr).await;
    let url = s.torrent.to_string_lossy().into_owned();
    let info = m.inspect_torrent(&url).await.expect("inspect");
    assert_eq!(info.name, "film.mkv");
    assert_eq!(info.size, data.len() as u64);

    let t = m
        .add(NewTask {
            url,
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");
    let done = wait_for(&m, t.id, Status::Done).await;
    // One file goes straight into the download folder.
    let path = done.path.expect("path");
    assert_eq!(path, w.0.join("downloads").join("film.mkv"));
    assert_eq!(std::fs::read(&path).expect("read"), data);
    assert_eq!(done.size, Some(data.len() as u64));
    m.close().await;
    s.session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_a_spoofing_name_as_it_reads() {
    let w = WorkDir::new("spoof");
    let source = w.0.join("source");
    std::fs::create_dir_all(&source).expect("mkdir");
    let data = content(3, 200_000);
    // Shows as `invoiceexe.pdf` where the override is honoured.
    let spoof = "invoice\u{202E}fdp.exe";
    std::fs::write(source.join(spoof), &data).expect("write");
    let s = seed(&source.join(spoof), &w.0.join("spoof.torrent")).await;

    let m = manager(&w.0, s.addr).await;
    let url = s.torrent.to_string_lossy().into_owned();
    let info = m.inspect_torrent(&url).await.expect("inspect");
    assert_eq!(info.files[0].name, "invoice_fdp.exe");
    let t = m
        .add(NewTask {
            url,
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");
    let done = wait_for(&m, t.id, Status::Done).await;
    let path = done.path.expect("path");
    assert_eq!(path, w.0.join("downloads").join("invoice_fdp.exe"));
    assert_eq!(std::fs::read(&path).expect("read"), data);
    assert!(!w.0.join("downloads").join(spoof).exists());
    m.close().await;
    s.session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_chosen_files_of_a_folder_and_resumes() {
    let w = WorkDir::new("folder");
    let source = w.0.join("source").join("Album");
    std::fs::create_dir_all(source.join("disc 2")).expect("mkdir");
    let a = content(2, 1 << 20);
    let b = content(3, 2 << 20);
    let c = content(4, 512 << 10);
    std::fs::write(source.join("01.flac"), &a).expect("write");
    std::fs::write(source.join("disc 2").join("02.flac"), &b).expect("write");
    std::fs::write(source.join("cover.jpg"), &c).expect("write");
    let s = seed(&source, &w.0.join("album.torrent")).await;

    let m = manager(&w.0, s.addr).await;
    let url = s.torrent.to_string_lossy().into_owned();
    let info = m.inspect_torrent(&url).await.expect("inspect");
    assert_eq!(info.name, "Album");
    let index = |name: &str| {
        info.files
            .iter()
            .position(|f| f.name == name)
            .unwrap_or_else(|| panic!("{name} not in {:?}", info.files))
    };
    // Both songs, not the cover.
    let files = vec![index("01.flac"), index("disc 2/02.flac")];

    let t = m
        .add(NewTask {
            url,
            start: true,
            stream: Some(Stream {
                torrent: Some(TorrentChoice {
                    files: Some(files),
                    folder: None,
                }),
                ..Stream::default()
            }),
            ..NewTask::default()
        })
        .await
        .expect("add");
    // Paused as soon as it moves, then resumed: the second start checks what is on disk.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let t = m.get(t.id).await.expect("get");
        if t.status == Status::Downloading && t.path.is_some() || t.status == Status::Done {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never started: {t:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    m.pause(t.id).await.expect("pause");
    wait_for(&m, t.id, Status::Paused).await;
    m.start(t.id).await.expect("resume");
    let done = wait_for(&m, t.id, Status::Done).await;

    let folder = w.0.join("downloads").join("Album");
    assert_eq!(done.path.as_deref(), Some(folder.as_path()));
    assert_eq!(done.size, Some((a.len() + b.len()) as u64));
    assert_eq!(std::fs::read(folder.join("01.flac")).expect("01"), a);
    assert_eq!(
        std::fs::read(folder.join("disc 2").join("02.flac")).expect("02"),
        b
    );
    // The cover was not chosen. librqbit may still write the pieces it shares with the
    // chosen files, but never the whole file.
    let cover = std::fs::metadata(folder.join("cover.jpg")).map_or(0, |m| m.len());
    let cover_data = std::fs::read(folder.join("cover.jpg")).unwrap_or_default();
    assert!(
        cover == 0 || cover_data != c,
        "the unchosen cover was downloaded"
    );

    // Removing with its files takes the torrent's folder.
    m.remove(t.id, true).await.expect("remove");
    assert!(!folder.exists());
    m.close().await;
    s.session.stop().await;
}

/// Over the internet: Ubuntu's server image. Its tracker hands out only Canonical's own seed,
/// so peers come from the DHT, as in the app. Reads the file list, downloads some 30 MB, then
/// removes it with its file. Run by hand:
/// `cargo test -p idma-manager --test torrent -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "uses the internet"]
async fn internet_torrent() {
    const URL: &str =
        "https://releases.ubuntu.com/noble/ubuntu-24.04.5-live-server-amd64.iso.torrent";
    if let Ok(filter) = std::env::var("RUST_LOG") {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    let w = WorkDir::new("internet");
    let m = Manager::open(
        &w.0.join("list.db"),
        Config {
            dir: w.0.join("downloads"),
            ..Config::default()
        },
    )
    .await
    .expect("open");
    let started = std::time::Instant::now();
    let info = m.inspect_torrent(URL).await.expect("inspect");
    println!(
        "{} · {} files · {} MB · read in {:?}",
        info.name,
        info.files.len(),
        info.size >> 20,
        started.elapsed()
    );
    let t = m
        .add(NewTask {
            url: URL.into(),
            start: true,
            ..NewTask::default()
        })
        .await
        .expect("add");
    let mut events = m.subscribe();
    let begun = std::time::Instant::now();
    let mut received = 0;
    while received < 30 << 20 {
        assert!(
            begun.elapsed() < Duration::from_secs(180),
            "too slow: {received} bytes"
        );
        if let Ok(Ok(idma_manager::Event::Progress(p))) =
            tokio::time::timeout(Duration::from_secs(1), events.recv()).await
            && p.id == t.id
        {
            received = p.received;
            if begun.elapsed().as_millis() % 2000 < 260 {
                println!(
                    "  {} MB · {:.1} MB/s · {} peers",
                    p.received >> 20,
                    p.speed / 1e6,
                    p.connections
                );
            }
        }
    }
    println!("30 MB in {:?}", begun.elapsed());
    let path = m.get(t.id).await.expect("get").path.expect("path");
    m.remove(t.id, true).await.expect("remove");
    assert!(!path.exists());
    m.close().await;
}
