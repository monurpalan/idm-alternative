//! Transport: HTTPS with HTTP/2, browser fingerprints, proxies.

use std::path::{Path, PathBuf};
use std::time::Duration;

use idma_engine::{Browser, CancellationToken, Error, Options, Outcome, Request, download};
use idma_testserver::{SelfSigned, Verdict, router, serve_plain, serve_tls, verify_file};

const MB: u64 = 1 << 20;
const LIMIT: Duration = Duration::from_secs(60);

async fn start_tls() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let cert = SelfSigned::generate().expect("certificate");
    tokio::spawn(async move { serve_tls(listener, router(), &cert).await });
    format!("https://localhost:{}", addr.port())
}

async fn start_plain() -> String {
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

async fn get(url: String, out: &Path, opts: &Options) -> idma_engine::Result<Outcome> {
    let req = Request {
        url,
        output: out.to_owned(),
        headers: Vec::new(),
    };
    tokio::time::timeout(LIMIT, download(&req, opts, None, &CancellationToken::new()))
        .await
        .expect("download hung")
}

/// The server's counters for the one URL a test used.
async fn stats(base: &str) -> serde_json::Value {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("client");
    let text = client
        .get(format!("{base}/_stats"))
        .send()
        .await
        .expect("stats")
        .text()
        .await
        .expect("stats body");
    let mut all: Vec<serde_json::Value> = serde_json::from_str(&text).expect("stats json");
    assert_eq!(all.len(), 1, "{all:?}");
    all.remove(0)
}

fn tls_opts() -> Options {
    Options {
        connections: 8,
        insecure: true,
        ..Options::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn http2_segments_get_their_own_connections() {
    let base = start_tls().await;
    let out = workdir("h2").join("f.bin");
    // 2 MiB/s per response: segmenting only helps if responses do not share a connection.
    let url = format!("{base}/file/f.bin?size={}&seed=4&rate=2M", 16 * MB);
    let done = get(url, &out, &tls_opts()).await.expect("download");
    assert_eq!(verify_file(&out, 4, 16 * MB).expect("verify"), Verdict::Ok);
    let s = stats(&base).await;
    assert_eq!(s["h2_gets"], s["gets"], "{s}");
    assert!(s["connections"].as_u64() >= Some(4), "{s}");
    assert!(done.elapsed < Duration::from_secs(4), "{:?}", done.elapsed);
}

#[tokio::test(flavor = "multi_thread")]
async fn presents_the_browser_user_agent() {
    let base = start_tls().await;
    let out = workdir("ua-chrome").join("f.bin");
    get(format!("{base}/file/f.bin?size={MB}"), &out, &tls_opts())
        .await
        .expect("download");
    let s = stats(&base).await;
    let ua = s["user_agent"].as_str().unwrap_or_default();
    assert!(ua.contains("Chrome/") && ua.contains("Windows"), "{ua}");
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_client_and_http1_only() {
    let base = start_tls().await;
    let out = workdir("plain").join("f.bin");
    let o = Options {
        browser: None,
        http1_only: true,
        ..tls_opts()
    };
    get(
        format!("{base}/file/f.bin?size={}&seed=2", 4 * MB),
        &out,
        &o,
    )
    .await
    .expect("download");
    assert_eq!(verify_file(&out, 2, 4 * MB).expect("verify"), Verdict::Ok);
    let s = stats(&base).await;
    assert_eq!(s["h2_gets"], 0, "{s}");
    let ua = s["user_agent"].as_str().unwrap_or_default();
    assert!(ua.starts_with("idm-alternative/"), "{ua}");
}

#[tokio::test(flavor = "multi_thread")]
async fn custom_user_agent_wins() {
    let base = start_tls().await;
    let out = workdir("ua-custom").join("f.bin");
    let o = Options {
        user_agent: Some("Wget/1.21".into()),
        browser: Some(Browser::Firefox),
        ..tls_opts()
    };
    get(format!("{base}/file/f.bin?size={MB}"), &out, &o)
        .await
        .expect("download");
    assert_eq!(stats(&base).await["user_agent"], "Wget/1.21");
}

#[tokio::test(flavor = "multi_thread")]
async fn self_signed_certificate_is_rejected_by_default() {
    let base = start_tls().await;
    let out = workdir("bad-cert").join("f.bin");
    let o = Options {
        insecure: false,
        ..tls_opts()
    };
    let err = get(format!("{base}/file/f.bin?size={MB}"), &out, &o)
        .await
        .expect_err("must fail");
    assert!(matches!(err, Error::Http(_)), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_through_a_proxy() {
    // The test server also answers absolute-form requests, so it can stand in for a
    // forward proxy: the file's host does not exist, only the proxy can reach "it".
    let proxy = start_plain().await;
    let out = workdir("proxy").join("f.bin");
    let o = Options {
        proxy: Some(proxy.clone()),
        connections: 4,
        ..Options::default()
    };
    let url = format!(
        "http://download.invalid/file/f.bin?size={}&seed=6&rate=4M",
        8 * MB
    );
    get(url, &out, &o).await.expect("download");
    assert_eq!(verify_file(&out, 6, 8 * MB).expect("verify"), Verdict::Ok);
    let s = stats(&proxy).await;
    assert!(s["gets"].as_u64() >= Some(2), "{s}");
}
