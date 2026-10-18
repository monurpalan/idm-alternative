//! End-to-end checks of every scenario parameter against a live server.

use std::time::{Duration, Instant};

use reqwest::{StatusCode, header};
use shardload_testserver::{content, router};

async fn start() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, router()).await });
    format!("http://{addr}")
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client")
}

fn expected(seed: u64, start: u64, len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    content::fill(seed, start, &mut v);
    v
}

#[tokio::test]
async fn full_download_matches_content() {
    let base = start().await;
    let r = client()
        .get(format!("{base}/file/a.bin?size=300K&seed=9"))
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()[header::ACCEPT_RANGES], "bytes");
    assert_eq!(r.headers()[header::CONTENT_LENGTH], "307200");
    let body = r.bytes().await.expect("body");
    assert_eq!(&body[..], expected(9, 0, 300 << 10));
}

#[tokio::test]
async fn range_returns_206_with_correct_slice() {
    let base = start().await;
    let r = client()
        .get(format!("{base}/file/a.bin?size=1M"))
        .header(header::RANGE, "bytes=1000-1999")
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        r.headers()[header::CONTENT_RANGE],
        "bytes 1000-1999/1048576"
    );
    let body = r.bytes().await.expect("body");
    assert_eq!(&body[..], expected(1, 1000, 1000));
}

#[tokio::test]
async fn unsatisfiable_range_is_416() {
    let base = start().await;
    let r = client()
        .get(format!("{base}/file/a.bin?size=1K"))
        .header(header::RANGE, "bytes=5000-")
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), StatusCode::RANGE_NOT_SATISFIABLE);
}

#[tokio::test]
async fn ranges_off_and_range_as_200_ignore_range() {
    let base = start().await;
    for q in ["ranges=off", "range_as_200=1"] {
        let r = client()
            .get(format!("{base}/file/a.bin?size=10K&{q}"))
            .header(header::RANGE, "bytes=100-")
            .send()
            .await
            .expect("send");
        assert_eq!(r.status(), StatusCode::OK, "{q}");
        assert_eq!(r.bytes().await.expect("body").len(), 10 << 10, "{q}");
    }
    let r = client()
        .get(format!("{base}/file/a.bin?size=10K&ranges=off"))
        .send()
        .await
        .expect("send");
    assert!(r.headers().get(header::ACCEPT_RANGES).is_none());
}

#[tokio::test]
async fn rotating_etag_breaks_if_range() {
    let base = start().await;
    let url = format!("{base}/file/a.bin?size=10K&etag=rotate");
    let first = client().get(&url).send().await.expect("send");
    let etag = first.headers()[header::ETAG]
        .to_str()
        .expect("etag")
        .to_owned();

    let r = client()
        .get(&url)
        .header(header::RANGE, "bytes=100-")
        .header(header::IF_RANGE, &etag)
        .send()
        .await
        .expect("send");
    assert_eq!(
        r.status(),
        StatusCode::OK,
        "changed validator must yield the full file"
    );
    assert_ne!(r.headers()[header::ETAG].to_str().expect("etag"), etag);
}

#[tokio::test]
async fn static_etag_satisfies_if_range() {
    let base = start().await;
    let url = format!("{base}/file/a.bin?size=10K");
    let etag = client().get(&url).send().await.expect("send").headers()[header::ETAG]
        .to_str()
        .expect("etag")
        .to_owned();
    let r = client()
        .get(&url)
        .header(header::RANGE, "bytes=100-")
        .header(header::IF_RANGE, &etag)
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
}

#[tokio::test]
async fn ttl_expires_link() {
    let base = start().await;
    let url = format!("{base}/file/a.bin?size=1K&ttl=1");
    assert_eq!(
        client().get(&url).send().await.expect("send").status(),
        StatusCode::OK
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(
        client().get(&url).send().await.expect("send").status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn html_error_after_n_requests() {
    let base = start().await;
    let url = format!("{base}/file/a.bin?size=1K&html_error_after=1");
    let ok = client().get(&url).send().await.expect("send");
    assert_eq!(
        ok.headers()[header::CONTENT_TYPE],
        "application/octet-stream"
    );
    let html = client().get(&url).send().await.expect("send");
    assert_eq!(html.status(), StatusCode::OK);
    assert!(
        html.headers()[header::CONTENT_TYPE]
            .to_str()
            .expect("ct")
            .starts_with("text/html")
    );
}

#[tokio::test]
async fn max_conns_rejects_extra_connection() {
    let base = start().await;
    let url = format!("{base}/file/a.bin?size=10M&rate=100K&max_conns=1");
    let held = client().get(&url).send().await.expect("send"); // keeps the slot while unread
    let extra = client().get(&url).send().await.expect("send");
    assert_eq!(extra.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(held);
}

#[tokio::test]
async fn refuse_status_forbids_extra_connection() {
    let base = start().await;
    let url = format!("{base}/file/a.bin?size=10M&rate=100K&max_conns=1&refuse_status=403");
    let held = client().get(&url).send().await.expect("send");
    let extra = client().get(&url).send().await.expect("send");
    assert_eq!(extra.status(), StatusCode::FORBIDDEN);
    drop(held);
}

#[tokio::test(flavor = "multi_thread")]
async fn total_rate_is_shared_by_connections() {
    let base = start().await;
    // 2 MiB at 1 MiB/s in total: about 2 s whether in one request or four.
    let url = format!("{base}/file/a.bin?size=2M&total_rate=1M");
    let started = std::time::Instant::now();
    let parts = (0..4u64).map(|i| {
        let (url, from, to) = (url.clone(), i * (512 << 10), (i + 1) * (512 << 10) - 1);
        tokio::spawn(async move {
            client()
                .get(&url)
                .header(header::RANGE, format!("bytes={from}-{to}"))
                .send()
                .await
                .expect("send")
                .bytes()
                .await
                .expect("body")
                .len()
        })
    });
    let mut got = 0;
    for p in parts {
        got += p.await.expect("join");
    }
    let secs = started.elapsed().as_secs_f64();
    assert_eq!(got, 2 << 20);
    assert!((1.7..3.0).contains(&secs), "took {secs:.2} s");
}

#[tokio::test]
async fn redirect_chain_ends_at_file() {
    let base = start().await;
    let r = client()
        .get(format!("{base}/file/a.bin?size=1K&redirect=2"))
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), StatusCode::FOUND);
    assert_eq!(
        r.headers()[header::LOCATION],
        "/file/a.bin?size=1K&redirect=1"
    );
}

#[tokio::test]
async fn drop_after_truncates_body() {
    let base = start().await;
    let r = client()
        .get(format!("{base}/file/a.bin?size=1M&drop_after=100K"))
        .send()
        .await
        .expect("send");
    assert!(
        r.bytes().await.is_err(),
        "body must fail after the injected drop"
    );
}

#[tokio::test]
async fn stall_after_stops_sending() {
    let base = start().await;
    let r = client()
        .get(format!("{base}/file/a.bin?size=1M&stall_after=64K"))
        .send()
        .await
        .expect("send");
    let res = tokio::time::timeout(Duration::from_millis(500), r.bytes()).await;
    assert!(res.is_err(), "body must hang after stall point");
}

#[tokio::test]
async fn no_length_uses_chunked_without_ranges() {
    let base = start().await;
    let r = client()
        .get(format!("{base}/file/a.bin?size=100K&no_length=1"))
        .send()
        .await
        .expect("send");
    assert!(r.headers().get(header::CONTENT_LENGTH).is_none());
    assert!(r.headers().get(header::ACCEPT_RANGES).is_none());
    assert_eq!(r.bytes().await.expect("body").len(), 100 << 10);
}

#[tokio::test]
async fn rate_limit_is_respected() {
    let base = start().await;
    let started = Instant::now();
    let r = client()
        .get(format!("{base}/file/a.bin?size=200K&rate=400K"))
        .send()
        .await
        .expect("send");
    r.bytes().await.expect("body");
    let secs = started.elapsed().as_secs_f64();
    assert!((0.4..0.8).contains(&secs), "200K at 400K/s took {secs:.2}s");
}

#[tokio::test]
async fn content_disposition_has_utf8_name() {
    let base = start().await;
    let r = client()
        .get(format!("{base}/file/a.bin?size=1K&cd=%C3%A7ay.zip"))
        .send()
        .await
        .expect("send");
    let cd = r.headers()[header::CONTENT_DISPOSITION]
        .to_str()
        .expect("cd");
    assert!(cd.contains("filename*=UTF-8''%C3%A7ay.zip"), "{cd}");
}

#[tokio::test]
async fn stats_count_gets_and_peak() {
    let base = start().await;
    let url = format!("{base}/file/a.bin?size=1K");
    for _ in 0..3 {
        client()
            .get(&url)
            .send()
            .await
            .expect("send")
            .bytes()
            .await
            .expect("body");
    }
    let stats: Vec<serde_json::Value> = client()
        .get(format!("{base}/_stats"))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0]["gets"], 3);
    assert_eq!(stats[0]["bytes_sent"], 3 * 1024);
}

#[test]
fn verify_file_detects_problems() {
    use shardload_testserver::{Verdict, verify_file};
    let dir = std::env::temp_dir().join(format!("shardload-verify-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("f.bin");

    std::fs::write(&path, expected(5, 0, 5000)).expect("write");
    assert_eq!(verify_file(&path, 5, 5000).expect("verify"), Verdict::Ok);
    assert_eq!(
        verify_file(&path, 5, 6000).expect("verify"),
        Verdict::WrongLength {
            expected: 6000,
            actual: 5000
        }
    );
    let mut data = expected(5, 0, 5000);
    data[4321] ^= 1;
    std::fs::write(&path, data).expect("write");
    assert_eq!(
        verify_file(&path, 5, 5000).expect("verify"),
        Verdict::Mismatch { offset: 4321 }
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn serves_h2c_prior_knowledge() {
    let base = start().await;
    let h2 = reqwest::Client::builder()
        .http2_prior_knowledge()
        .build()
        .expect("client");
    let r = h2
        .get(format!("{base}/file/a.bin?size=64K"))
        .header(header::RANGE, "bytes=0-1023")
        .send()
        .await
        .expect("send");
    assert_eq!(r.version(), reqwest::Version::HTTP_2);
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(&r.bytes().await.expect("body")[..], expected(1, 0, 1024));
}
