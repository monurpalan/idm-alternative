//! Without `-o`, `dm` saves under the name the server suggests, in `-d` or the current folder.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

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

fn dm(url: &str, dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_dm"));
    c.args(["get", url, "-q", "-c", "2", "-d"])
        .arg(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

async fn run(url: &str, dir: &Path) {
    let mut c = dm(url, dir);
    let status = tokio::task::spawn_blocking(move || c.status())
        .await
        .expect("join")
        .expect("run dm");
    assert!(status.success(), "{status}");
}

/// A finished file is never replaced: the second copy gets a number. An interrupted download
/// under the server's name resumes instead of starting a numbered copy.
#[tokio::test(flavor = "multi_thread")]
async fn existing_files_get_numbered_and_interrupted_ones_resume() {
    let base = start().await;
    let dir = std::env::temp_dir()
        .join("idma-cli-tests")
        .join(format!("collide-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create dir");
    let size = 1 << 20;

    let url = format!("{base}/file/a.tar.gz?size={size}&seed=4");
    run(&url, &dir).await;
    run(&url, &dir).await;
    for name in ["a.tar.gz", "a (1).tar.gz"] {
        assert_eq!(
            verify_file(&dir.join(name), 4, size).expect("verify"),
            Verdict::Ok,
            "{name}"
        );
    }

    // 8 MiB at 2 × 1 MiB/s: killed after the first checkpoint, long before the end.
    let size = 8 << 20;
    let url = format!("{base}/file/slow.bin?size={size}&seed=6&rate=1M");
    let mut child = dm(&url, &dir).spawn().expect("spawn");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "finished too early"
    );
    child.kill().expect("kill");
    child.wait().expect("wait");
    assert!(dir.join("slow.bin.idma-state").exists());
    run(&url, &dir).await;
    assert_eq!(
        verify_file(&dir.join("slow.bin"), 6, size).expect("verify"),
        Verdict::Ok
    );
    assert!(!dir.join("slow (1).bin").exists());
    std::fs::remove_dir_all(&dir).ok();
}
