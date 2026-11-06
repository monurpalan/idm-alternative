//! The site grabber against a small site served by the test: pages linking to pages and
//! files, a loop, a broken page, and links to other sites.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::{Html, IntoResponse};
use shardload_engine::{CancellationToken, Options};
use shardload_manager::grabber::{GrabOptions, grab};

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

/// A password-protected folder linking to a page on another site: the login opens the
/// folder and is not sent to the other site. `127.0.0.1` and `localhost` are two sites.
#[tokio::test(flavor = "multi_thread")]
async fn a_login_opens_its_site_and_goes_nowhere_else() {
    let other_saw_login = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saw = Arc::clone(&other_saw_login);
    let other = Router::new().fallback(move |headers: axum::http::HeaderMap| {
        let saw = Arc::clone(&saw);
        async move {
            if headers.contains_key("authorization") {
                saw.store(true, Ordering::Relaxed);
            }
            Html(r#"<a href="/elsewhere.zip">Elsewhere</a>"#)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let other_port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, other).await });

    let login = shardload_engine::basic_auth("user", "secret");
    let wanted = login.1.clone();
    let folder = Router::new().fallback(move |headers: axum::http::HeaderMap| {
        let wanted = wanted.clone();
        async move {
            let given = headers.get("authorization").and_then(|v| v.to_str().ok());
            if given != Some(wanted.as_str()) {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            Html(format!(
                r#"<a href="private.zip">Private</a>
                   <a href="http://localhost:{other_port}/page.html">Other site</a>"#
            ))
            .into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move { axum::serve(listener, folder).await });

    let opts = GrabOptions {
        depth: 2,
        same_site: false,
        ..GrabOptions::default()
    };
    let found = grab(
        &format!("{base}/"),
        &opts,
        &[login],
        &engine(),
        |_| {},
        &CancellationToken::new(),
    )
    .await
    .expect("grab");
    let urls: Vec<&str> = found.iter().map(|f| f.url.as_str()).collect();
    assert!(urls.iter().any(|u| u.ends_with("/private.zip")), "{urls:?}");
    assert!(
        urls.iter().any(|u| u.ends_with("/elsewhere.zip")),
        "{urls:?}"
    );
    assert!(!other_saw_login.load(Ordering::Relaxed));
}

/// The app's grabber uses the login saved for the site.
#[tokio::test(flavor = "multi_thread")]
async fn the_login_saved_for_a_site_is_used() {
    let wanted = shardload_engine::basic_auth("user", "secret").1;
    let folder = Router::new().fallback(move |headers: axum::http::HeaderMap| {
        let wanted = wanted.clone();
        async move {
            match headers.get("authorization").and_then(|v| v.to_str().ok()) {
                Some(given) if given == wanted => {
                    Html(r#"<a href="private.zip">Private</a>"#).into_response()
                }
                _ => StatusCode::UNAUTHORIZED.into_response(),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, folder).await });

    let dir = std::env::temp_dir().join(format!("shardload-grab-login-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("dir");
    let m = shardload_manager::Manager::open(
        &dir.join("list.db"),
        shardload_manager::Config {
            dir: dir.join("downloads"),
            engine: engine(),
            ..shardload_manager::Config::default()
        },
    )
    .await
    .expect("open");
    let url = format!("http://{addr}/");
    let cancel = CancellationToken::new();
    let refused = m
        .grab_site(&url, &GrabOptions::default(), |_| {}, &cancel)
        .await;
    assert!(refused.is_err(), "no login yet");
    m.save_login(&addr.to_string(), "user", "secret")
        .await
        .expect("save");
    let found = m
        .grab_site(&url, &GrabOptions::default(), |_| {}, &cancel)
        .await
        .expect("grab");
    assert_eq!(found.len(), 1);
    assert!(found[0].url.ends_with("/private.zip"));
    m.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

/// A download page that sends the browser on with a refresh, and one whose button is a
/// script: the files are found; the page's own script and picture are not.
#[tokio::test(flavor = "multi_thread")]
async fn refreshes_and_scripts_lead_to_files() {
    let page = Router::new().fallback(|| async {
        Html(
            r#"<html><head>
               <meta http-equiv="Refresh" content="5; URL='/files/setup.exe'">
               <script src="https://cdn.invalid/app.js"></script>
               <script>
                 var player = "https://cdn.invalid/player.js";
                 var logo = 'https://cdn.invalid/logo.png';
                 document.getElementById("go").onclick = function () {
                   location.href = 'https://dl.invalid/Tool-2.1.msi';
                 };
                 var data = {"file":"https:\/\/dl.invalid\/pack.zip"};
               </script></head><body><button id="go">Download</button></body></html>"#,
        )
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move { axum::serve(listener, page).await });
    let mut found = names(&base, GrabOptions::default()).await;
    found.sort();
    assert_eq!(
        found,
        [
            "/files/setup.exe",
            "https://dl.invalid/Tool-2.1.msi",
            "https://dl.invalid/pack.zip"
        ]
    );
}
