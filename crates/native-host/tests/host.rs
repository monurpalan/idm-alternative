//! The host as the browser runs it: frames on stdin, replies on stdout, the app at the other
//! end of the pipe (here a stand-in served by the test).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use shardload_bridge::{Reply, Request, read_json, write_json};
use tokio::process::{Child, Command};

/// A data folder of its own: the pipe name and token follow from it.
struct Profile(PathBuf);

impl Profile {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir()
            .join("shardload-host-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create");
        Self(dir)
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Stands in for the app: answers ping with a version, downloads with the URL as error text
/// (so the test sees what arrived).
fn serve_app(dir: &Path) {
    let token = shardload_bridge::new_token(dir).expect("token");
    let pipe = shardload_bridge::pipe_name(dir);
    tokio::spawn(async move {
        shardload_bridge::serve(&pipe, token, |req| async move {
            match req {
                Request::Ping => Reply {
                    version: Some("test".into()),
                    ..Reply::ok()
                },
                Request::Download(c) => Reply {
                    error: Some(format!("{} {:?}", c.url, c.request_headers())),
                    ..Reply::ok()
                },
                _ => Reply::error("unexpected"),
            }
        })
        .await
        .ok();
    });
}

fn host(dir: &Path, app: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_shardload-host"))
        .env("SHARDLOAD_HOME", dir)
        .env("SHARDLOAD_APP", app)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn host")
}

async fn ask(child: &mut Child, msg: &serde_json::Value) -> Reply {
    let stdin = child.stdin.as_mut().expect("stdin");
    write_json(stdin, msg).await.expect("write");
    let stdout = child.stdout.as_mut().expect("stdout");
    tokio::time::timeout(Duration::from_secs(30), read_json::<Reply, _>(stdout))
        .await
        .expect("host hung")
        .expect("read")
        .expect("reply")
}

#[tokio::test(flavor = "multi_thread")]
async fn passes_messages_to_the_app_and_back() {
    let p = Profile::new("pass");
    serve_app(&p.0);
    let mut h = host(&p.0, "unused");
    let pong = ask(&mut h, &serde_json::json!({ "type": "ping" })).await;
    assert!(pong.ok);
    assert_eq!(pong.version.as_deref(), Some("test"));

    // Several messages over one host process and pipe connection.
    let r = ask(
        &mut h,
        &serde_json::json!({
            "type": "download",
            "url": "https://example.com/ç.zip",
            "cookies": "sid=1",
            "referrer": "https://example.com/page"
        }),
    )
    .await;
    let echoed = r.error.expect("echo");
    assert!(echoed.starts_with("https://example.com/ç.zip"), "{echoed}");
    assert!(echoed.contains("\"Cookie\", \"sid=1\""), "{echoed}");
    assert!(echoed.contains("\"Referer\""), "{echoed}");

    // Closing stdin ends the host cleanly.
    drop(h.stdin.take());
    let status = tokio::time::timeout(Duration::from_secs(10), h.wait())
        .await
        .expect("host did not exit")
        .expect("wait");
    assert!(status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn extension_cannot_send_the_token_and_bad_input_is_answered() {
    let p = Profile::new("hello");
    serve_app(&p.0);
    let mut h = host(&p.0, "unused");
    let r = ask(
        &mut h,
        &serde_json::json!({ "type": "hello", "token": "x" }),
    )
    .await;
    assert!(!r.ok);
    // Listing, adding without asking and removing are for the user's own tools.
    for msg in [
        serde_json::json!({ "type": "tasks" }),
        serde_json::json!({ "type": "add", "url": "https://x/a.zip" }),
        serde_json::json!({ "type": "remove", "ids": [1], "delete_files": true }),
    ] {
        let r = ask(&mut h, &msg).await;
        assert!(!r.ok);
        assert!(r.error.expect("error").contains("not accepted"));
    }
    let r = ask(&mut h, &serde_json::json!({ "type": "nonsense" })).await;
    assert!(!r.ok);
    assert!(r.error.expect("error").contains("bad message"));
    // Still working afterwards.
    assert!(ask(&mut h, &serde_json::json!({ "type": "ping" })).await.ok);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_token_is_refused() {
    let p = Profile::new("token");
    serve_app(&p.0);
    // The token on disk no longer matches the running app's.
    std::fs::write(p.0.join(shardload_bridge::TOKEN_FILE), "0".repeat(64)).expect("write");
    let mut h = host(&p.0, r"C:\nonexistent\shardload.exe");
    let r = ask(&mut h, &serde_json::json!({ "type": "ping" })).await;
    assert!(!r.ok, "{r:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn waits_for_an_app_that_is_starting() {
    let p = Profile::new("late");
    // Nothing listens yet. The host "starts the app" (a harmless stand-in program), then
    // keeps trying; the app comes up a second later.
    let stand_in = if cfg!(windows) {
        r"C:\Windows\System32\whoami.exe"
    } else {
        "/bin/true"
    };
    let mut h = host(&p.0, stand_in);
    let dir = p.0.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        serve_app(&dir);
    });
    let r = ask(&mut h, &serde_json::json!({ "type": "ping" })).await;
    assert!(r.ok, "{r:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_an_app_that_cannot_be_started() {
    let p = Profile::new("missing");
    let mut h = host(&p.0, r"C:\nonexistent\shardload.exe");
    let r = ask(&mut h, &serde_json::json!({ "type": "ping" })).await;
    assert!(!r.ok);
    assert!(
        r.error.expect("error").contains("could not start the app"),
        "should say why"
    );
}
