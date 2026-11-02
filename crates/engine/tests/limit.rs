//! Speed limits. Kept in their own test binary, which cargo runs after the others, and run
//! one at a time: accuracy is measured in wall-clock time.
//!
//! Rates are measured from the downloads' progress, from the first byte (or the moment the
//! limit changed) to the end: asking the server first takes what time it takes, more on a
//! busy machine, and is not the limit's doing. A stretch in the middle would not do:
//! connections pay for what they read after reading it, so what has arrived runs ahead of
//! the limit by what they still owe, more as connections are added; nothing is owed before
//! the first byte or when the limit changes, and all of it is paid by the end.
//!
//! A limit is never to be passed: a few percent over it fails. A machine too busy to read
//! its sockets in time can stay under it, so the bound below only catches a limit applied
//! far too strictly.

mod common;

use std::time::{Duration, Instant};

use common::*;
use shardload_engine::{CancellationToken, Limiter, Options, Outcome, Progress, Request, download};
use tokio::sync::watch;

/// Bytes received so far, summed over the downloads, at moments 10 ms apart; the last one is
/// when the downloads ended.
type Samples = Vec<(Instant, u64)>;

/// Runs the downloads together, sampling their progress, and calls `on_sample` with the
/// bytes received so far at every sample (to change a limit once some have arrived).
async fn run(
    jobs: &[(Request, Options)],
    mut on_sample: impl FnMut(u64),
) -> (Vec<shardload_engine::Result<Outcome>>, Samples) {
    let channels: Vec<_> = jobs
        .iter()
        .map(|_| watch::channel(Progress::default()))
        .collect();
    let receivers: Vec<_> = channels.iter().map(|(_, rx)| rx.clone()).collect();
    let cancel = CancellationToken::new();
    let downloads = futures_util::future::join_all(
        jobs.iter()
            .zip(&channels)
            .map(|((req, opts), (tx, _))| download(req, opts, Some(tx), &cancel)),
    );
    let mut samples = Samples::new();
    let sampling = async {
        loop {
            let received = receivers.iter().map(|rx| rx.borrow().received).sum();
            on_sample(received);
            samples.push((Instant::now(), received));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let done = tokio::time::timeout(LIMIT, async {
        tokio::select! {
            done = downloads => done,
            () = sampling => unreachable!(),
        }
    })
    .await
    .expect("download hung");
    let total = receivers.iter().map(|rx| rx.borrow().received).sum();
    samples.push((Instant::now(), total));
    (done, samples)
}

/// Close enough to the limit: never over it by more than the error of measuring.
fn at_limit(ratio: f64) -> bool {
    (0.85..=1.05).contains(&ratio)
}

/// Bytes per second from the first moment `from` bytes had arrived (the bytes of that moment
/// counted when `from` is 1, the first byte) to the first moment `to` had (`None`: the end),
/// as a fraction of `rate`.
fn ratio(samples: &Samples, from: u64, to: Option<u64>, rate: u64) -> f64 {
    let at = |bytes: u64| {
        *samples
            .iter()
            .find(|(_, b)| *b >= bytes)
            .unwrap_or_else(|| panic!("{bytes} bytes never arrived"))
    };
    let (t0, b0) = at(from);
    let b0 = if from == 1 { 0 } else { b0 };
    let (t1, b1) = to.map_or_else(|| *samples.last().expect("samples"), at);
    (b1 - b0) as f64 / (t1 - t0).as_secs_f64() / rate as f64
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
    let (done, samples) = run(&[(request(url, &out), o)], |_| {}).await;
    done.into_iter().for_each(|d| drop(d.expect("download")));
    assert_verified(&out, 7, size);
    let r = ratio(&samples, 1, None, rate);
    assert!(at_limit(r), "{r:.3} of the limit");
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
    let (size, rate) = (16 * MB, 4 * MB);
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
        (req, o)
    };
    let (done, samples) = run(&[job("a.bin"), job("b.bin")], |_| {}).await;
    done.into_iter().for_each(|d| drop(d.expect("download")));
    assert_verified(&dir.join("a.bin"), 7, size);
    assert_verified(&dir.join("b.bin"), 7, size);
    let r = ratio(&samples, 1, None, rate);
    assert!(at_limit(r), "{r:.3} of the limit");
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
    let size = 24 * MB;
    let limiter = Limiter::new(Some(2 * MB));
    let o = Options {
        limiters: vec![limiter.clone()],
        ..opts(8)
    };
    let req = request(format!("{base}/file/f.bin?size={size}&seed=7"), &out);
    // 2 MiB/s for the first 5 MiB, then 8 MiB/s.
    let (done, samples) = run(&[(req, o)], |received| {
        if received >= 5 * MB {
            limiter.set_rate(Some(8 * MB));
        }
    })
    .await;
    done.into_iter().for_each(|d| drop(d.expect("download")));
    assert_verified(&out, 7, size);
    let before = ratio(&samples, MB, Some(5 * MB), 2 * MB);
    assert!(at_limit(before), "{before:.3} of 2 MiB/s");
    let after = ratio(&samples, 5 * MB, None, 8 * MB);
    assert!(at_limit(after), "{after:.3} of 8 MiB/s");
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
        split: shardload_engine::segments::SplitPolicy {
            min_piece: 16 << 10,
            ..Default::default()
        },
        ..opts(16)
    };
    let (tx, mut rx) = watch::channel(Progress::default());
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

/// Bytes the server sent beyond the file, as a fraction of it.
fn waste(sent: u64, size: u64) -> f64 {
    sent as f64 / size as f64 - 1.0
}

#[tokio::test(flavor = "multi_thread")]
async fn limit_with_slow_connections_wastes_little() {
    let _serial = serial().await;
    let base = start().await;
    let dir = workdir("limit-slow-conns");
    let out = dir.join("f.bin");
    // Each connection gets 1 MiB/s, so the 4 MiB/s limit needs several; every one of them is
    // throttled and fills the buffers to the server. Requests used to cover whole segments,
    // and splitting one threw away what it had buffered: 8–37 % extra.
    let (size, rate) = (24 * MB, 4 * MB);
    let o = Options {
        limiters: vec![Limiter::new(Some(rate))],
        ..opts(16)
    };
    let url = format!("{base}/file/f.bin?size={size}&seed=7&rate={}", MB);
    let (done, samples) = run(&[(request(url, &out), o)], |_| {}).await;
    done.into_iter().for_each(|d| drop(d.expect("download")));
    assert_verified(&out, 7, size);
    let r = ratio(&samples, 1, None, rate);
    assert!(at_limit(r), "{r:.3} of the limit");
    let w = waste(bytes_sent(&base).await, size);
    assert!(w < 0.08, "{:.1} % extra", w * 100.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn limit_set_during_a_download_wastes_little() {
    let _serial = serial().await;
    let base = start().await;
    let dir = workdir("limit-set-later");
    let out = dir.join("f.bin");
    // 16 connections at 512 KiB/s each, then, once 6 MiB have arrived, a 2 MiB/s limit. The
    // open requests asked for whole segments; throttled, their buffers used to fill and get
    // thrown away (44 % extra).
    let (size, rate) = (24 * MB, 2 * MB);
    let limiter = Limiter::new(None);
    let o = Options {
        limiters: vec![limiter.clone()],
        ..opts(16)
    };
    let req = request(
        format!("{base}/file/f.bin?size={size}&seed=7&rate={}", 512 << 10),
        &out,
    );
    let (done, samples) = run(&[(req, o)], |received| {
        if received >= 6 * MB {
            limiter.set_rate(Some(rate));
        }
    })
    .await;
    done.into_iter().for_each(|d| drop(d.expect("download")));
    assert_verified(&out, 7, size);
    let r = ratio(&samples, 6 * MB, None, rate);
    assert!(at_limit(r), "{r:.3} of the limit");
    let w = waste(bytes_sent(&base).await, size);
    assert!(w < 0.08, "{:.1} % extra", w * 100.0);
}
