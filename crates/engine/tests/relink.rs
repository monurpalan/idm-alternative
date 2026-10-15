//! Expiring links: redirect targets resolved again, expired links stopping cleanly, and
//! continuing an interrupted download from a fresh link.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use idma_engine::{CancellationToken, Error, Options, Outcome, Request, download, patch_url};
use idma_testserver::{Verdict, router, serve_plain, verify_file};

const MB: u64 = 1 << 20;
const SIZE: u64 = 16 * MB;
const LIMIT: Duration = Duration::from_secs(60);

async fn start() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { serve_plain(listener, router()).await });
    format!("http://{addr}")
}

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("idma-engine-tests")
        .join(format!("{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

fn request(url: String, output: &Path) -> Request {
    Request {
        url,
        output: output.to_owned(),
        headers: Vec::new(),
    }
}

fn opts() -> Options {
    Options {
        connections: 4,
        checkpoint_every: Duration::from_millis(200),
        ..Options::default()
    }
}

async fn get(url: String, out: &Path) -> idma_engine::Result<Outcome> {
    tokio::time::timeout(
        LIMIT,
        download(&request(url, out), &opts(), None, &CancellationToken::new()),
    )
    .await
    .expect("download hung")
}

fn state_file(out: &Path) -> PathBuf {
    PathBuf::from(format!("{}.idma-state", out.display()))
}

/// Starts a download whose link dies after a second: responses are cut every MiB, so new
/// requests keep coming and the first one after a second is refused.
async fn expire(base: &str, out: &Path) {
    let url = format!("{base}/file/old.bin?size={SIZE}&seed=8&rate=1M&ttl=1&drop_after=1M");
    let started = Instant::now();
    let err = get(url, out).await.expect_err("the link expires");
    assert!(matches!(err, Error::LinkExpired(403)), "{err}");
    // Fatal at once, not after a series of retries with backoff.
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert!(state_file(out).exists(), "progress must be kept");
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_redirect_target_is_resolved_again() {
    let base = start().await;
    let out = workdir("redirect-expiry").join("f.bin");
    // Every resolved link lives one second; the download needs about four, and responses
    // are cut every MiB so new requests keep hitting the expiry.
    let url = format!(
        "{base}/file/f.bin?size={SIZE}&seed=8&rate=1M&ttl=1&drop_after=1M&redirect=1&redirect_unique=1"
    );
    get(url, &out).await.expect("download");
    assert_eq!(verify_file(&out, 8, SIZE).expect("verify"), Verdict::Ok);
    let stats = reqwest::get(format!("{base}/_stats"))
        .await
        .expect("stats")
        .text()
        .await
        .expect("stats body");
    let targets = stats.matches("hop=").count();
    assert!(
        targets >= 3,
        "only {targets} redirect targets used: {stats}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn fresh_link_continues_an_expired_download() {
    let base = start().await;
    let out = workdir("fresh-link").join("f.bin");
    expire(&base, &out).await;
    // Another URL for the same file, as the browser would capture it.
    let fresh = format!("{base}/file/new.bin?size={SIZE}&seed=8");
    let done = get(fresh, &out).await.expect("resume");
    assert!(
        done.resumed_from >= 2 * MB,
        "resumed from {}",
        done.resumed_from
    );
    assert_eq!(verify_file(&out, 8, SIZE).expect("verify"), Verdict::Ok);
}

#[tokio::test(flavor = "multi_thread")]
async fn link_to_a_different_file_starts_over() {
    let base = start().await;
    let out = workdir("other-file").join("f.bin");
    expire(&base, &out).await;
    // Same size, different content: the sample on disk does not match.
    let other = format!("{base}/file/new.bin?size={SIZE}&seed=9");
    let done = get(other, &out).await.expect("download");
    assert_eq!(done.resumed_from, 0);
    assert_eq!(verify_file(&out, 9, SIZE).expect("verify"), Verdict::Ok);
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_url_checks_the_new_link() {
    let base = start().await;
    let out = workdir("patch").join("f.bin");
    expire(&base, &out).await;

    let wrong = request(format!("{base}/file/new.bin?size={SIZE}&seed=9"), &out);
    let err = patch_url(&wrong, &opts())
        .await
        .expect_err("different file");
    assert!(matches!(err, Error::FileChanged), "{err}");
    let shorter = request(
        format!("{base}/file/new.bin?size={}&seed=8", SIZE - 1),
        &out,
    );
    assert!(patch_url(&shorter, &opts()).await.is_err());

    let fresh = format!("{base}/file/new.bin?size={SIZE}&seed=8");
    patch_url(&request(fresh.clone(), &out), &opts())
        .await
        .expect("same file");
    let state = std::fs::read_to_string(state_file(&out)).expect("state");
    assert!(
        state.contains("seed=8\"") && state.contains("new.bin"),
        "{state}"
    );
    let done = get(fresh, &out).await.expect("resume");
    assert!(done.resumed_from >= 2 * MB);
    assert_eq!(verify_file(&out, 8, SIZE).expect("verify"), Verdict::Ok);
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_url_without_an_interrupted_download_fails() {
    let base = start().await;
    let out = workdir("patch-nothing").join("f.bin");
    let req = request(format!("{base}/file/new.bin?size={SIZE}"), &out);
    assert!(matches!(
        patch_url(&req, &opts()).await,
        Err(Error::State(_))
    ));
}
