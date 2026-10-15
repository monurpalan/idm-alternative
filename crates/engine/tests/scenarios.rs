//! The engine against every hostile server behaviour the test server can produce.
//!
//! Each test starts its own server in-process, downloads into its own directory, and checks
//! the result byte-for-byte against the generated content.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use idma_engine::{
    CancellationToken, Error, Limiter, Options, Outcome, Progress, Request, download,
};
use idma_testserver::{Verdict, router, verify_file};

const MB: u64 = 1 << 20;
/// No test may hang the suite.
const LIMIT: Duration = Duration::from_secs(60);

async fn start() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, router()).await });
    format!("http://{addr}")
}

/// A fresh, empty directory for one test.
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

fn opts(connections: usize) -> Options {
    Options {
        connections,
        ..Options::default()
    }
}

async fn get(req: &Request, opts: &Options) -> idma_engine::Result<Outcome> {
    tokio::time::timeout(LIMIT, download(req, opts, None, &CancellationToken::new()))
        .await
        .expect("download hung")
}

fn assert_verified(path: &Path, seed: u64, size: u64) {
    assert_eq!(verify_file(path, seed, size).expect("verify"), Verdict::Ok);
    assert!(!PathBuf::from(format!("{}.idma-part", path.display())).exists());
    assert!(!PathBuf::from(format!("{}.idma-state", path.display())).exists());
}

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

/// Bytes the server sent for every URL so far.
async fn bytes_sent(base: &str) -> u64 {
    let text = reqwest::get(format!("{base}/_stats"))
        .await
        .expect("stats")
        .text()
        .await
        .expect("stats body");
    let stats: Vec<serde_json::Value> = serde_json::from_str(&text).expect("stats json");
    stats.iter().filter_map(|u| u["bytes_sent"].as_u64()).sum()
}

/// Average speed of `bytes` over `elapsed`, as a fraction of `rate`.
fn ratio(bytes: u64, elapsed: Duration, rate: u64) -> f64 {
    bytes as f64 / elapsed.as_secs_f64() / rate as f64
}

#[tokio::test(flavor = "multi_thread")]
async fn speed_limit_is_accurate() {
    let base = start().await;
    let out = workdir("limit").join("f.bin");
    let (size, rate) = (16 * MB, 4 * MB);
    let url = format!("{base}/file/f.bin?size={size}&seed=7");
    let o = Options {
        limiters: vec![Limiter::new(Some(rate))],
        ..opts(8)
    };
    let done = get(&request(url, &out), &o).await.expect("download");
    assert_verified(&out, 7, size);
    let r = ratio(size, done.elapsed, rate);
    assert!((0.95..=1.05).contains(&r), "{r:.3} of the limit");
    // Under a limit, every abandoned response wastes what its socket buffered (megabytes on
    // loopback). Alone this download needs one connection and wastes nothing; with the other
    // tests loading the CPU a few steals still happen. The bound catches the old behaviour,
    // which sent the file twice.
    let sent = bytes_sent(&base).await;
    assert!(
        sent < size + size / 4,
        "server sent {sent} bytes for {size}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn global_limit_is_shared_between_downloads() {
    let base = start().await;
    let dir = workdir("global-limit");
    let (size, rate) = (8 * MB, 4 * MB);
    let global = Limiter::new(Some(rate));
    let job = |name: &str| {
        let req = request(
            format!("{base}/file/{name}?size={size}&seed=7"),
            &dir.join(name),
        );
        let o = Options {
            // Each download's own limit is higher; the shared one decides.
            limiters: vec![global.clone(), Limiter::new(Some(3 * rate))],
            ..opts(8)
        };
        async move { get(&req, &o).await }
    };
    let started = Instant::now();
    let (a, b) = tokio::join!(job("a.bin"), job("b.bin"));
    let elapsed = started.elapsed();
    a.expect("a");
    b.expect("b");
    assert_verified(&dir.join("a.bin"), 7, size);
    assert_verified(&dir.join("b.bin"), 7, size);
    let r = ratio(2 * size, elapsed, rate);
    assert!((0.95..=1.05).contains(&r), "{r:.3} of the limit");
    let sent = bytes_sent(&base).await;
    assert!(
        sent < 2 * size + size / 2,
        "server sent {sent} bytes for {}",
        2 * size
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn limit_can_change_during_a_download() {
    let base = start().await;
    let out = workdir("limit-change").join("f.bin");
    let size = 12 * MB;
    let limiter = Limiter::new(Some(2 * MB));
    let o = Options {
        limiters: vec![limiter.clone()],
        ..opts(8)
    };
    let req = request(format!("{base}/file/f.bin?size={size}&seed=7"), &out);
    let raise = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        limiter.set_rate(Some(8 * MB));
    };
    // ~2 MiB in the first second, the other 10 MiB at 8 MiB/s: about 2.25 s.
    let (done, ()) = tokio::join!(get(&req, &o), raise);
    let elapsed = done.expect("download").elapsed;
    assert_verified(&out, 7, size);
    assert!(
        (2.0..2.6).contains(&elapsed.as_secs_f64()),
        "took {elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn low_limit_uses_few_connections() {
    let base = start().await;
    let out = workdir("low-limit").join("f.bin");
    // 96 KiB/s allows three connections of at least 32 KiB/s each.
    let (size, rate) = (192 << 10, 96 << 10);
    let o = Options {
        limiters: vec![Limiter::new(Some(rate))],
        split: idma_engine::segments::SplitPolicy {
            min_piece: 16 << 10,
            ..Default::default()
        },
        ..opts(16)
    };
    let (tx, mut rx) = tokio::sync::watch::channel(Progress::default());
    let req = request(format!("{base}/file/f.bin?size={size}&seed=7"), &out);
    let watch = async move {
        let mut peak = 0;
        while rx.changed().await.is_ok() {
            peak = peak.max(rx.borrow().connections);
        }
        peak
    };
    let run = async move {
        let r = download(&req, &o, Some(&tx), &CancellationToken::new()).await;
        drop(tx);
        r
    };
    let (done, peak) = tokio::time::timeout(LIMIT, async { tokio::join!(run, watch) })
        .await
        .expect("hung");
    done.expect("download");
    assert_verified(&out, 7, size);
    assert!((1..=3).contains(&peak), "peak {peak} connections");
}
