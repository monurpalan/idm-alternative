//! Downloads without a checksum, checked by their own structure, against the test server: a
//! byte the server sends damaged once is found, only its range is downloaded again, and the
//! file comes out right.

use std::path::PathBuf;
use std::time::Duration;

use shardload_manager::{Config, Manager, NewTask, Status, Task};
use shardload_testserver::samples::sample;

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
        .join(format!("structure-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("workdir");
    dir
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let t = m.get(t.id).await.expect("get");
        if matches!(t.status, Status::Done | Status::Error) {
            return t;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out: {t:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

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

fn position(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len())
        .position(|w| w == needle)
        .expect("in the sample")
}

/// A byte whose damage the kind's checks find.
fn damage_at(kind: &str, file: &[u8]) -> usize {
    match kind {
        "zip" => position(file, b"data/big.bin") + 200,
        "png" => position(file, b"IDAT") + (64 << 10) + 500,
        "pdf" => {
            let object = position(file, b"8 0 obj");
            object + position(&file[object..], b"stream\n") + 1000
        }
        "mp4" => position(file, b"moov") - 2,
        "mkv" => position(file, &[0x1F, 0x43, 0xB6, 0x75]) + (256 << 10) + 2000,
        _ => file.len() / 2,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_byte_is_found_and_only_its_range_downloaded_again() {
    let server = serve().await;
    let dir = workdir("repair");
    let m = Manager::open(
        &dir.join("list.db"),
        Config {
            dir: dir.join("downloads"),
            // One connection: the response with the damaged byte is read to the end (with
            // more, another connection may take that range over and get it right).
            connections: 1,
            engine: shardload_engine::Options {
                find_checksum: false,
                ..shardload_engine::Options::default()
            },
            // Its requests would count in what the server sent.
            skeleton_first: false,
            ..Config::default()
        },
    )
    .await
    .expect("open");
    for (kind, ext, format) in [
        ("zip", "zip", "ZIP"),
        ("gz", "gz", "gzip"),
        ("xz", "xz", "xz"),
        ("png", "png", "PNG"),
        ("pdf", "pdf", "PDF"),
        ("mp4", "mp4", "MP4"),
        ("mkv", "mkv", "Matroska"),
    ] {
        let want = sample(kind, 1).expect(kind);
        let at = damage_at(kind, &want);
        let name = format!("bad-{kind}.{ext}");
        // The manager asks about the file first (the first request): the download is the second.
        let url = format!("{server}/sample/{name}?kind={kind}&corrupt_at={at}&corrupt_get=2");
        let t = download(&m, url).await;
        assert_eq!(t.status, Status::Done, "{kind}: {t:?}");
        let got = std::fs::read(t.path.as_ref().expect("path")).expect("file");
        assert!(got == want, "{kind}: the file is not right");
        let s = t
            .structure
            .clone()
            .unwrap_or_else(|| panic!("{kind}: not checked"));
        assert_eq!((s.format.as_str(), s.damaged), (format, false), "{kind}");
        assert!(s.repaired > 0, "{kind}: nothing downloaded again");
        // The file twice (this server counts the first look, which the client drops, as
        // the whole file) and the damaged range again: nothing else.
        let extra = sent(&server, &name).await - 2 * want.len() as u64;
        assert_eq!(
            extra, s.repaired,
            "{kind}: {extra} bytes more than the file"
        );
        println!(
            "{kind}: {} bytes, {} downloaded again",
            want.len(),
            s.repaired
        );

        // The same file sent right: checked, nothing downloaded again.
        let name = format!("good-{kind}.{ext}");
        let t = download(&m, format!("{server}/sample/{name}?kind={kind}")).await;
        let s = t.structure.clone().expect("checked");
        assert_eq!((s.repaired, s.damaged), (0, false), "{kind}");
        assert_eq!(sent(&server, &name).await, 2 * want.len() as u64, "{kind}");
    }
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}
