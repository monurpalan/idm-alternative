//! One file from several addresses: mirrors that serve the same bytes share the work, and
//! one that serves another file, breaks or crawls is left out without failing the download.
//! Every "server" here is a URL of the test server with its own limits and statistics.

mod common;

use std::time::Instant;

use common::*;
use idma_engine::Request;

/// Bytes the server sent for each URL whose path contains `name`.
async fn sent_for(base: &str, name: &str) -> u64 {
    let text = reqwest::get(format!("{base}/_stats"))
        .await
        .expect("stats")
        .text()
        .await
        .expect("stats body");
    let stats: Vec<serde_json::Value> = serde_json::from_str(&text).expect("stats json");
    stats
        .iter()
        .filter(|u| u["url"].as_str().is_some_and(|url| url.contains(name)))
        .filter_map(|u| u["bytes_sent"].as_u64())
        .sum()
}

fn with_mirrors(url: String, mirrors: Vec<String>, out: &std::path::Path) -> Request {
    Request {
        mirrors,
        ..request(url, out)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mirrors_add_up_when_each_server_is_limited() {
    let _serial = serial().await;
    let base = start().await;
    let dir = workdir("mirrors-speed");
    let out = dir.join("f.bin");
    // Each server gives a client 2 MiB/s however many connections it opens.
    let (size, q) = (12 * MB, "size=12M&seed=21&total_rate=2M");
    let req = with_mirrors(
        format!("{base}/file/a.bin?{q}"),
        vec![
            format!("{base}/file/b.bin?{q}"),
            format!("{base}/file/c.bin?{q}"),
        ],
        &out,
    );
    let started = Instant::now();
    get(&req, &opts(8)).await.expect("download");
    let elapsed = started.elapsed().as_secs_f64();
    assert_verified(&out, 21, size);
    // Alone it would take 6 s; three servers at 2 MiB/s, about 2 s.
    assert!(elapsed < 3.5, "took {elapsed:.2} s");
    for name in ["a.bin", "b.bin", "c.bin"] {
        let sent = sent_for(&base, name).await;
        assert!(sent > 2 * MB, "{name} sent only {sent} bytes");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mirror_of_another_file_is_left_out() {
    let base = start().await;
    let dir = workdir("mirrors-other");
    let out = dir.join("f.bin");
    let size = 4 * MB;
    let req = with_mirrors(
        format!("{base}/file/a.bin?size=4M&seed=22"),
        vec![
            // Same size, other bytes.
            format!("{base}/file/other.bin?size=4M&seed=99"),
            // Another size.
            format!("{base}/file/small.bin?size=3M&seed=22"),
            // No ranges.
            format!("{base}/file/whole.bin?size=4M&seed=22&ranges=0"),
            // Nothing there.
            "http://127.0.0.1:9/file/a.bin".to_owned(),
        ],
        &out,
    );
    get(&req, &opts(8)).await.expect("download");
    assert_verified(&out, 22, size);
    // Each was asked once for the start of the file, never for the download. A server
    // without ranges answers with the whole file; what it sends before the answer is
    // dropped is lost, but it is not used.
    for name in ["other.bin", "small.bin"] {
        let sent = sent_for(&base, name).await;
        assert!(sent <= 128 << 10, "{name} sent {sent} bytes");
    }
    let sent = sent_for(&base, "whole.bin").await;
    assert!(sent < size / 2, "whole.bin sent {sent} bytes");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mirror_that_breaks_is_dropped() {
    let base = start().await;
    let dir = workdir("mirrors-break");
    let out = dir.join("f.bin");
    let size = 16 * MB;
    let q = "size=16M&seed=23&rate=2M";
    let req = with_mirrors(
        format!("{base}/file/a.bin?{q}"),
        vec![
            // Starts sending an error page after the checks and a few ranges.
            format!("{base}/file/html.bin?{q}&html_error_after=4"),
            // Starts refusing (403) a second after the first request.
            format!("{base}/file/expiring.bin?{q}&ttl=1"),
        ],
        &out,
    );
    get(&req, &opts(8)).await.expect("download");
    assert_verified(&out, 23, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crawling_mirror_is_dropped() {
    let _serial = serial().await;
    let base = start().await;
    let dir = workdir("mirrors-slow");
    let out = dir.join("f.bin");
    let size = 16 * MB;
    let req = with_mirrors(
        format!("{base}/file/a.bin?size=16M&seed=24&rate=4M"),
        vec![format!("{base}/file/slow.bin?size=16M&seed=24&rate=32K")],
        &out,
    );
    let started = Instant::now();
    get(&req, &opts(8)).await.expect("download");
    let elapsed = started.elapsed().as_secs_f64();
    assert_verified(&out, 24, size);
    // 8 connections at 4 MiB/s finish in about half a second; a mirror at 32 KiB/s that
    // kept its ranges would hold the end back for many seconds.
    assert!(elapsed < 4.0, "took {elapsed:.2} s");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mirror_takes_over_when_the_url_fails() {
    let base = start().await;
    let dir = workdir("mirrors-takeover");
    let out = dir.join("f.bin");
    let size = 4 * MB;
    // A mirror network hands out addresses before every mirror has the file: the first
    // one answers 404.
    let req = with_mirrors(
        "http://127.0.0.1:9/missing.bin".to_owned(),
        vec![format!("{base}/file/a.bin?size=4M&seed=25")],
        &out,
    );
    get(&req, &opts(8)).await.expect("download");
    assert_verified(&out, 25, size);
}
