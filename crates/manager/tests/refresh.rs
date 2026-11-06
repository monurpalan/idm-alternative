//! A link that expires part way, refreshed from the page the download came from.

mod common;

use common::*;
use shardload_manager::{Manager, NewTask, Status};

/// The test server, and `/page.html` linking to `fresh`.
async fn site(fresh: String) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let page = format!(
        r#"<html><body><a href="{}">Download again</a></body></html>"#,
        fresh.replace('&', "&amp;")
    );
    let app = shardload_testserver::router().merge(axum::Router::new().route(
        "/page.html",
        axum::routing::get(move || async move { axum::response::Html(page) }),
    ));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
    base
}

/// Expires after a second; its connections are cut every MiB, so it asks again soon.
const EXPIRING: &str = "size=8M&seed=51&rate=2M&drop_after=1M&ttl=1";

#[tokio::test(flavor = "multi_thread")]
async fn an_expired_link_continues_with_a_fresh_one_from_its_page() {
    let fresh_path = "/file/f.bin?size=8M&seed=51&rate=8M&k=fresh";
    // The page's link is relative: resolved against the page.
    let base = site(fresh_path.to_owned()).await;
    let w = WorkDir::new("refresh");
    let m = Manager::open(&w.db(), w.one_connection())
        .await
        .expect("open");
    let task = m
        .add(NewTask {
            headers: vec![("Referer".into(), format!("{base}/page.html"))],
            ..new_task(format!("{base}/file/f.bin?{EXPIRING}"))
        })
        .await
        .expect("add");
    let done = wait_done(&m, task.id).await;
    assert_eq!(done.url, format!("{base}{fresh_path}"));
    assert_file(&done, 51, 8 * MB);
    m.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_on_the_page_to_another_file_is_not_taken() {
    // Same name, other content (seed): the bytes on disk tell them apart.
    let other = "/file/f.bin?size=8M&seed=52&rate=8M";
    let base = site(other.to_owned()).await;
    let w = WorkDir::new("refresh-other");
    let m = Manager::open(&w.db(), w.one_connection())
        .await
        .expect("open");
    let task = m
        .add(NewTask {
            headers: vec![("Referer".into(), format!("{base}/page.html"))],
            ..new_task(format!("{base}/file/f.bin?{EXPIRING}"))
        })
        .await
        .expect("add");
    let ended = wait_for(&m, task.id, 60, |t| t.status == Status::Error).await;
    assert!(ended.url.contains("ttl=1"), "{}", ended.url);
    m.close().await;
}
