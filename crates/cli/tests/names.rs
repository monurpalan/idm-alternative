//! Without `-o`, `dm` saves under the name the server suggests, in `-d` or the current folder.

use std::process::{Command, Stdio};

use idma_testserver::{Verdict, router, verify_file};

async fn start() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, router()).await });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_under_server_name() {
    let base = start().await;
    let dir = std::env::temp_dir()
        .join("idma-cli-tests")
        .join(format!("names-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create dir");
    let size = 1 << 20;

    for (url, want) in [
        (
            format!("{base}/file/x.bin?size={size}&seed=3&cd=..%2F%C3%A7ay.zip"),
            "çay.zip",
        ),
        (
            format!("{base}/file/plain.bin?size={size}&seed=3"),
            "plain.bin",
        ),
    ] {
        let d = dir.clone();
        let status = tokio::task::spawn_blocking(move || {
            Command::new(env!("CARGO_BIN_EXE_dm"))
                .args(["get", &url, "-q", "-d"])
                .arg(&d)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
        })
        .await
        .expect("join")
        .expect("run dm");
        assert!(status.success(), "{status}");
        assert_eq!(
            verify_file(&dir.join(want), 3, size).expect("verify"),
            Verdict::Ok,
            "{want}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}
