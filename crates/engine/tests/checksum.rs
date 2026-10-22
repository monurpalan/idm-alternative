//! Verifying the finished file against a checksum the user supplied, or one published next
//! to it.

mod common;

use sha2::{Digest, Sha256};
use shardload_engine::{CancellationToken, Checksum, Error, Options, Phase, Progress, download};
use shardload_testserver::content::fill;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{LIMIT, MB, assert_verified, get, opts, request, start, workdir};

/// The test server, plus a `SHA256SUMS` next to its files with `sums` in it.
async fn start_with_sums(sums: String) -> String {
    start_with_sums_counted(sums).await.0
}

/// Like [`start_with_sums`], also counting the requests for `SHA256SUMS`.
async fn start_with_sums_counted(sums: String) -> (String, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked);
    let app = shardload_testserver::router().merge(axum::Router::new().route(
        "/file/SHA256SUMS",
        axum::routing::get(move || async move {
            counter.fetch_add(1, Ordering::Relaxed);
            sums
        }),
    ));
    (serve(app).await, asked)
}

async fn serve(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{addr}")
}

/// The paths (with query) the test server's file route was asked for.
async fn asked_for(base: &str) -> Vec<String> {
    let text = reqwest::get(format!("{base}/_stats"))
        .await
        .expect("stats")
        .text()
        .await
        .expect("stats body");
    let stats: Vec<serde_json::Value> = serde_json::from_str(&text).expect("stats json");
    stats
        .iter()
        .filter_map(|u| u["url"].as_str().map(str::to_owned))
        .collect()
}

fn searching(connections: usize) -> Options {
    Options {
        find_checksum: true,
        ..opts(connections)
    }
}

fn hex_of(checksum: &str) -> &str {
    checksum.split_once(':').expect("algorithm").1
}

/// SHA-256 of the test server's content for `seed` and `size`.
fn sha256_of(seed: u64, size: u64) -> String {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; MB as usize];
    let mut offset = 0;
    while offset < size {
        let n = (size - offset).min(MB) as usize;
        fill(seed, offset, &mut buf[..n]);
        h.update(&buf[..n]);
        offset += n as u64;
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

#[tokio::test(flavor = "multi_thread")]
async fn matching_checksum_passes_and_reports_verifying() {
    let base = start().await;
    let dir = workdir("checksum-ok");
    let out = dir.join("f.bin");
    let size = 8 * MB;
    let req = request(format!("{base}/file/f.bin?size={size}&seed=11"), &out);
    let mut o = opts(4);
    o.checksum = Some(sha256_of(11, size).parse().expect("checksum"));

    let (tx, mut rx) = tokio::sync::watch::channel(Progress::default());
    let watcher = tokio::spawn(async move {
        let mut phases = Vec::new();
        while rx.changed().await.is_ok() {
            let phase = rx.borrow_and_update().phase;
            if phases.last() != Some(&phase) {
                phases.push(phase);
            }
        }
        phases
    });
    let done = tokio::time::timeout(
        LIMIT,
        download(&req, &o, Some(&tx), &CancellationToken::new()),
    )
    .await
    .expect("hung")
    .expect("download");
    drop(tx);
    assert_eq!(done.size, size);
    assert_verified(&out, 11, size);
    let phases = watcher.await.expect("join");
    assert!(phases.contains(&Phase::Verifying), "{phases:?}");
    assert_eq!(phases.last(), Some(&Phase::Done), "{phases:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn mismatch_fails_and_keeps_the_file() {
    let base = start().await;
    let dir = workdir("checksum-bad");
    let out = dir.join("f.bin");
    let size = 2 * MB;
    let req = request(format!("{base}/file/f.bin?size={size}&seed=12"), &out);
    let mut o = opts(4);
    let wrong: Checksum = sha256_of(13, size).parse().expect("checksum");
    o.checksum = Some(wrong.clone());
    match get(&req, &o).await {
        Err(Error::ChecksumMismatch {
            expected,
            actual,
            origin: None,
        }) => {
            assert_eq!(expected, wrong.to_string());
            assert_eq!(actual, sha256_of(12, size));
        }
        other => panic!("expected a mismatch, got {other:?}"),
    }
    // The file is there for inspection; nothing is left to resume.
    assert_verified(&out, 12, size);
    assert!(!dir.join("f.bin.shardload-state").exists());
    assert!(!dir.join("f.bin.shardload-part").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn checksum_found_in_sha256sums_is_checked() {
    let size = 3 * MB;
    let good = sha256_of(21, size);
    let other = sha256_of(22, MB);
    let base = start_with_sums(format!(
        "{}  other.bin\r\n{} *./f.bin\r\n",
        hex_of(&other),
        hex_of(&good)
    ))
    .await;
    let dir = workdir("checksum-found");
    let out = dir.join("f.bin");
    let req = request(format!("{base}/file/f.bin?size={size}&seed=21"), &out);

    let (tx, mut rx) = tokio::sync::watch::channel(Progress::default());
    let watcher = tokio::spawn(async move {
        let mut from = None;
        while rx.changed().await.is_ok() {
            let p = rx.borrow_and_update();
            if p.phase == Phase::Verifying {
                from.clone_from(&p.checksum_from);
            }
        }
        from
    });
    let done = tokio::time::timeout(
        LIMIT,
        download(&req, &searching(4), Some(&tx), &CancellationToken::new()),
    )
    .await
    .expect("hung")
    .expect("download");
    drop(tx);
    assert_verified(&out, 21, size);
    assert_eq!(done.checksum.map(|c| c.to_string()), Some(good));
    assert_eq!(done.checksum_from.as_deref(), Some("SHA256SUMS"));
    assert_eq!(
        watcher.await.expect("join").as_deref(),
        Some("SHA256SUMS"),
        "progress names the checksum's source while verifying"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_checksum_found_in_sha256sums_fails_like_a_given_one() {
    let size = 2 * MB;
    let wrong = sha256_of(24, size);
    let base = start_with_sums(format!("{}  f.bin\n", hex_of(&wrong))).await;
    let dir = workdir("checksum-found-bad");
    let out = dir.join("f.bin");
    let req = request(format!("{base}/file/f.bin?size={size}&seed=23"), &out);
    match get(&req, &searching(4)).await {
        Err(e) => {
            assert!(e.to_string().contains("checksum from SHA256SUMS"), "{e}");
            let Error::ChecksumMismatch {
                expected,
                actual,
                origin,
            } = e
            else {
                panic!("expected a mismatch, got {e:?}");
            };
            assert_eq!(expected, wrong);
            assert_eq!(actual, sha256_of(23, size));
            assert_eq!(origin.as_deref(), Some("SHA256SUMS"));
        }
        other => panic!("expected a mismatch, got {other:?}"),
    }
    // Kept for inspection, as with a given checksum.
    assert_verified(&out, 23, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_published_checksum_is_no_checksum() {
    // The plain test server answers every name under /file/ with a large binary file, so
    // each candidate checksum file is something to turn down.
    let base = start().await;
    let dir = workdir("checksum-none");
    let out = dir.join("f.bin");
    let size = MB;
    let req = request(format!("{base}/file/f.bin?size={size}&seed=25"), &out);
    let done = get(&req, &searching(2)).await.expect("download");
    assert_verified(&out, 25, size);
    assert!(done.checksum.is_none() && done.checksum_from.is_none());

    // Turned off, nothing is looked for even where a list exists.
    let base = start_with_sums(format!("{}  g.bin\n", hex_of(&sha256_of(1, 1)))).await;
    let out = dir.join("g.bin");
    let req = request(format!("{base}/file/g.bin?size={size}&seed=26"), &out);
    let done = get(&req, &opts(2)).await.expect("download");
    assert_verified(&out, 26, size);
    assert!(done.checksum.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn small_files_are_not_searched_for() {
    let size = 512 << 10;
    let (base, asked) =
        start_with_sums_counted(format!("{}  s.bin\n", hex_of(&sha256_of(1, 1)))).await;
    let dir = workdir("checksum-small");
    let out = dir.join("s.bin");
    let req = request(format!("{base}/file/s.bin?size={size}&seed=27"), &out);
    let done = get(&req, &searching(2)).await.expect("download");
    assert_verified(&out, 27, size);
    assert!(done.checksum.is_none());
    assert_eq!(asked.load(Ordering::Relaxed), 0);
    assert_eq!(asked_for(&base).await.len(), 1, "only the file itself");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_is_searched_once() {
    let size = 2 * MB;
    let (base, asked) = start_with_sums_counted(format!(
        "{}  a.bin\n{}  b.bin\n",
        hex_of(&sha256_of(28, size)),
        hex_of(&sha256_of(29, size))
    ))
    .await;
    let dir = workdir("checksum-folder");
    for (name, seed) in [("a.bin", 28), ("b.bin", 29)] {
        let out = dir.join(name);
        let req = request(format!("{base}/file/{name}?size={size}&seed={seed}"), &out);
        let done = get(&req, &searching(2)).await.expect("download");
        assert_verified(&out, seed, size);
        assert_eq!(done.checksum_from.as_deref(), Some("SHA256SUMS"));
    }
    // The list was read once; the second file's own checksum files were not asked for,
    // since the first file had none.
    assert_eq!(asked.load(Ordering::Relaxed), 1);
    let urls = asked_for(&base).await;
    assert!(
        !urls.iter().any(|u| u.starts_with("/file/b.bin.")),
        "{urls:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_refuses_is_asked_no_more() {
    let app = shardload_testserver::router().merge(axum::Router::new().route(
        "/file/r.bin.sha256",
        axum::routing::get(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }),
    ));
    let base = serve(app).await;
    let dir = workdir("checksum-refused");
    let size = 2 * MB;
    for (name, seed) in [("r.bin", 30), ("q.bin", 31)] {
        let out = dir.join(name);
        let req = request(format!("{base}/file/{name}?size={size}&seed={seed}"), &out);
        let done = get(&req, &searching(2)).await.expect("download");
        assert_verified(&out, seed, size);
        assert!(done.checksum.is_none());
    }
    // At most the requests already in flight with the refused one (three at a time), and
    // nothing for the next file of the folder.
    let urls = asked_for(&base).await;
    let extra: Vec<_> = urls
        .iter()
        .filter(|u| {
            !u.contains('?') && !u.ends_with("/r.bin.sha512") && !u.ends_with("/SHA256SUMS")
        })
        .collect();
    assert!(extra.is_empty(), "{urls:?}");
}

/// The SHA-256 of every `piece` bytes of the test server's content.
fn piece_hashes(seed: u64, size: u64, piece: u64) -> shardload_engine::Pieces {
    let mut hashes = Vec::new();
    let mut at = 0;
    while at < size {
        let mut buf = vec![0u8; piece.min(size - at) as usize];
        fill(seed, at, &mut buf);
        let digest: String = Sha256::digest(&buf)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        hashes.push(format!("sha256:{digest}"));
        at += piece;
    }
    shardload_engine::Pieces {
        length: piece,
        hashes,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_corrupt_piece_is_downloaded_again_alone() {
    let base = start().await;
    let dir = workdir("pieces");
    let out = dir.join("f.bin");
    let size = 8 * MB;
    // One byte in the sixth MiB comes corrupt the first time.
    let url = format!(
        "{base}/file/f.bin?size={size}&seed=21&corrupt_at={}",
        5 * MB + 12345
    );
    let o = Options {
        pieces: Some(piece_hashes(21, size, MB)),
        ..opts(4)
    };
    get(&request(url, &out), &o).await.expect("download");
    assert_verified(&out, 21, size);
    // The file, then the one piece again: not the whole file twice. (A connection moved to
    // another range on a busy machine may add a little.)
    let sent = common::bytes_sent(&base).await;
    assert!((size + MB..size + 2 * MB).contains(&sent), "sent {sent}");

    // Without piece hashes the same corruption is only caught by the whole-file checksum.
    let out = dir.join("g.bin");
    let url = format!(
        "{base}/file/g.bin?size={size}&seed=21&corrupt_at={}",
        5 * MB + 12345
    );
    let mut whole = vec![0u8; size as usize];
    fill(21, 0, &mut whole);
    let digest: String = Sha256::digest(&whole)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let o = Options {
        checksum: Some(format!("sha256:{digest}").parse().expect("checksum")),
        ..opts(4)
    };
    match get(&request(url, &out), &o).await {
        Err(Error::ChecksumMismatch { .. }) => {}
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pieces_that_stay_corrupt_fail_the_download() {
    let base = start().await;
    let dir = workdir("pieces-bad");
    let out = dir.join("f.bin");
    let size = 4 * MB;
    let url = format!("{base}/file/f.bin?size={size}&seed=22");
    // Hashes of other content: every piece is "corrupt", and stays so.
    let o = Options {
        pieces: Some(piece_hashes(99, size, MB)),
        ..opts(4)
    };
    match get(&request(url, &out), &o).await {
        Err(Error::State(m)) => assert!(m.contains("still corrupt"), "{m}"),
        other => panic!("{other:?}"),
    }
}
