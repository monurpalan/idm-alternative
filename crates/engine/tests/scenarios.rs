//! The engine against every hostile server behaviour the test server can produce.
//!
//! Each test starts its own server in-process, downloads into its own directory, and checks
//! the result byte-for-byte against the generated content.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::*;
use idma_engine::{CancellationToken, Error, Options, Outcome, download};

/// Downloads `query` (appended to the URL) and checks the file.
async fn ok(name: &str, size: u64, query: &str, connections: usize) -> Outcome {
    let base = start().await;
    let out = workdir(name).join("f.bin");
    let url = format!("{base}/file/f.bin?size={size}&seed=7{query}");
    let done = get(&request(url, &out), &opts(connections))
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(done.size, size);
    assert_verified(&out, 7, size);
    done
}

#[tokio::test(flavor = "multi_thread")]
async fn clean() {
    ok("clean", 64 * MB, "", 16).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tiny_and_empty_files() {
    ok("one-byte", 1, "", 16).await;
    ok("below-min-piece", 100 << 10, "", 16).await;
    ok("empty", 0, "", 16).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn per_connection_limit_uses_parallelism() {
    // 8 connections at 2 MiB/s: about 1 s; one connection would need 8 s.
    let done = ok("per-conn", 16 * MB, "&rate=2M", 8).await;
    assert!(done.elapsed < Duration::from_secs(5), "{:?}", done.elapsed);
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_connection_does_not_hold_the_tail() {
    // The third connection crawls at 32 KiB/s; it must not keep a large share of the file.
    let done = ok("slow-tail", 32 * MB, "&rate=4M&slow_conn=3:32K", 8).await;
    assert!(done.elapsed < Duration::from_secs(6), "{:?}", done.elapsed);
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_cap_is_respected() {
    ok("conn-cap", 16 * MB, "&rate=4M&max_conns=3", 16).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dropped_connections_are_resumed() {
    ok("flaky", 16 * MB, "&rate=16M&drop_after=1M", 8).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stalled_connection_is_replaced() {
    let base = start().await;
    let out = workdir("stall").join("f.bin");
    let url = format!("{base}/file/f.bin?size={}&seed=7&stall_after=2M", 8 * MB);
    let o = Options {
        read_timeout: Duration::from_millis(500),
        ..opts(4)
    };
    get(&request(url, &out), &o).await.expect("download");
    assert_verified(&out, 7, 8 * MB);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_range_support_uses_one_connection() {
    ok("no-ranges", 8 * MB, "&ranges=off", 16).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fake_range_support_falls_back_to_one_connection() {
    ok("fake-ranges", 8 * MB, "&range_as_200=1&rate=16M", 16).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_length() {
    ok("no-length", 8 * MB, "&no_length=1", 16).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn follows_redirects() {
    ok("redirect", 8 * MB, "&redirect=3&rate=8M", 8).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_file_is_detected_and_discarded() {
    let base = start().await;
    let out = workdir("etag-rotate").join("f.bin");
    let url = format!("{base}/file/f.bin?size={}&etag=rotate&rate=4M", 16 * MB);
    let err = get(&request(url, &out), &opts(8))
        .await
        .expect_err("must fail");
    assert!(matches!(err, Error::FileChanged), "{err}");
    assert!(!out.exists());
    assert!(!PathBuf::from(format!("{}.idma-part", out.display())).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn html_error_page_is_not_written_as_data() {
    let base = start().await;
    let out = workdir("html").join("f.bin");
    let url = format!(
        "{base}/file/f.bin?size={}&html_error_after=1&rate=4M",
        16 * MB
    );
    let err = get(&request(url, &out), &opts(8))
        .await
        .expect_err("must fail");
    assert!(matches!(err, Error::HtmlResponse), "{err}");
    assert!(!out.exists());
}

/// Downloads at a limited rate and interrupts the download after `after`, either cleanly
/// (`cancel`) or by dropping it mid-flight like a killed process would.
async fn interrupt(url: &str, out: &Path, after: Duration, cancel: bool) {
    let o = Options {
        checkpoint_every: Duration::from_millis(200),
        ..opts(4)
    };
    let token = CancellationToken::new();
    let req = request(url.to_owned(), out);
    let run = download(&req, &o, None, &token);
    tokio::pin!(run);
    tokio::select! {
        r = &mut run => panic!("finished before the interruption: {r:?}"),
        () = tokio::time::sleep(after) => {}
    }
    if cancel {
        token.cancel();
        let r = tokio::time::timeout(LIMIT, run).await.expect("cancel hung");
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    }
    // Otherwise `run` is dropped here without a final checkpoint.
}

async fn interrupted_then_resumed(name: &str, cancel: bool) {
    let base = start().await;
    let out = workdir(name).join("f.bin");
    let size = 16 * MB;
    // 4 connections at 1 MiB/s: about 4 s in total.
    let url = format!("{base}/file/f.bin?size={size}&seed=3&rate=1M");
    interrupt(&url, &out, Duration::from_millis(1500), cancel).await;
    assert!(PathBuf::from(format!("{}.idma-state", out.display())).exists());

    let started = Instant::now();
    let done = get(&request(url, &out), &opts(4)).await.expect("resume");
    assert!(
        done.resumed_from >= 2 * MB,
        "resumed from only {} bytes",
        done.resumed_from
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_verified(&out, 3, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_then_resume() {
    interrupted_then_resumed("cancel-resume", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn killed_then_resume() {
    interrupted_then_resumed("kill-resume", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn completed_download_is_not_fetched_again() {
    let base = start().await;
    let out = workdir("again").join("f.bin");
    let url = format!("{base}/file/f.bin?size={}&seed=7", 4 * MB);
    get(&request(url.clone(), &out), &opts(4))
        .await
        .expect("first");
    // The output exists and no state is left: a new run downloads afresh and still verifies.
    get(&request(url, &out), &opts(4)).await.expect("second");
    assert_verified(&out, 7, 4 * MB);
}
