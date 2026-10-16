//! Speed limits. Kept in their own test binary, which cargo runs after the others, and run
//! one at a time: accuracy is measured in wall-clock time.

mod common;

use std::time::{Duration, Instant};

use common::*;
use idma_engine::{CancellationToken, Limiter, Options, Progress, download};

/// Average speed of `bytes` over `elapsed`, as a fraction of `rate`.
fn ratio(bytes: u64, elapsed: Duration, rate: u64) -> f64 {
    bytes as f64 / elapsed.as_secs_f64() / rate as f64
}

#[tokio::test(flavor = "multi_thread")]
async fn speed_limit_is_accurate() {
    let _serial = serial().await;
    let base = start().await;
    let dir = workdir("limit");
    let out = dir.join("f.bin");
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
    let _serial = serial().await;
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
    let _serial = serial().await;
    let base = start().await;
    let dir = workdir("limit-change");
    let out = dir.join("f.bin");
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
    let _serial = serial().await;
    let base = start().await;
    let dir = workdir("low-limit");
    let out = dir.join("f.bin");
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
