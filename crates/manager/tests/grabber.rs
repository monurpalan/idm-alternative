//! The site grabber against a small site served by the test: pages linking to pages and
//! files, a loop, a broken page, and links to other sites.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::{Html, IntoResponse};
use idma_engine::{CancellationToken, Options};
use idma_manager::grabber::{GrabOptions, grab};

#[derive(Default)]
struct Hits {
    pages: AtomicUsize,
    files: AtomicUsize,
}

async fn site(State(hits): State<Arc<Hits>>, uri: Uri) -> impl IntoResponse {
    let page = |body: &str| {
        hits.pages.fetch_add(1, Ordering::Relaxed);
        Html(format!("<html><body>{body}</body></html>")).into_response()
    };
    match uri.path() {
        "/" => page(
            r#"<a href="a.zip">Archive A</a>
               <a href="/docs/">Docs</a>
               <a href="/broken.html">Broken</a>
               <a href="http://elsewhere.invalid/page.html">Elsewhere</a>
               <img src="https://cdn.invalid/logo.png">"#,
        ),
        "/docs/" => page(
            r#"<a href="b.pdf">Manual</a>
               <a href="/">Home</a>
               <a href="deep.html">Deeper</a>
               <a href="../a.zip">Archive A again</a>"#,
        ),
        "/docs/deep.html" => page(r#"<a href="/media/c.mp4">Film</a>"#),
        "/broken.html" => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        _ => {
            hits.files.fetch_add(1, Ordering::Relaxed);
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

async fn serve() -> (String, Arc<Hits>) {
    let hits = Arc::new(Hits::default());
    let app = Router::new().fallback(site).with_state(Arc::clone(&hits));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await });
    (format!("http://{addr}"), hits)
}

fn engine() -> Options {
    Options {
        browser: None,
        max_failures: 0,
        ..Options::default()
    }
}

async fn names(base: &str, opts: GrabOptions) -> Vec<String> {
    let found = grab(
        &format!("{base}/"),
        &opts,
        &[],
        &engine(),
        |_| {},
        &CancellationToken::new(),
    )
    .await
    .expect("grab");
    found.into_iter().map(|f| f.url.replace(base, "")).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_level_adds_the_files_of_the_pages_below() {
    let (base, hits) = serve().await;
    let depth = |depth| GrabOptions {
        depth,
        ..GrabOptions::default()
    };
    assert_eq!(
        names(&base, depth(1)).await,
        ["/a.zip", "https://cdn.invalid/logo.png"]
    );
    assert_eq!(
        names(&base, depth(2)).await,
        ["/a.zip", "https://cdn.invalid/logo.png", "/docs/b.pdf"]
    );
    assert_eq!(
        names(&base, depth(3)).await,
        [
            "/a.zip",
            "https://cdn.invalid/logo.png",
            "/docs/b.pdf",
            "/media/c.mp4"
        ]
    );
    // Files are listed, never downloaded; pages on other sites are not followed.
    assert_eq!(hits.files.load(Ordering::Relaxed), 0);
    // Depth 1 reads 1 page, 2 reads 3 (start, docs, broken), 3 reads 4: each once.
    assert_eq!(hits.pages.load(Ordering::Relaxed), 1 + 2 + 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_at_the_page_limit_and_reports_progress() {
    let (base, _) = serve().await;
    let opts = GrabOptions {
        depth: 3,
        max_pages: 2,
        ..GrabOptions::default()
    };
    let seen = std::sync::Mutex::new(Vec::new());
    let found = grab(
        &format!("{base}/"),
        &opts,
        &[],
        &engine(),
        |p| seen.lock().expect("lock").push(p.pages),
        &CancellationToken::new(),
    )
    .await
    .expect("grab");
    let seen = seen.into_inner().expect("lock");
    assert_eq!(seen.last(), Some(&2), "{seen:?}");
    assert!(!found.iter().any(|f| f.url.ends_with("c.mp4")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_page_that_cannot_be_read_is_an_error() {
    let (base, _) = serve().await;
    let err = grab(
        &format!("{base}/broken.html"),
        &GrabOptions::default(),
        &[],
        &engine(),
        |_| {},
        &CancellationToken::new(),
    )
    .await
    .expect_err("broken");
    assert!(err.contains("500"), "{err}");
}
