//! `dm` killed mid-download (TerminateProcess / SIGKILL, no chance to clean up) resumes
//! from its state file when run again, and the result is byte-identical.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use idma_testserver::{Verdict, router, verify_file};

const MB: u64 = 1 << 20;

async fn start() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, router()).await });
    format!("http://{addr}")
}

fn dm(url: &str, out: &PathBuf) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_dm"));
    c.args(["get", url, "-o"])
        .arg(out)
        .args(["-c", "4", "-q"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

#[tokio::test(flavor = "multi_thread")]
async fn killed_process_resumes() {
    let base = start().await;
    let dir = std::env::temp_dir()
        .join("idma-cli-tests")
        .join(format!("kill-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create dir");
    let out = dir.join("f.bin");
    let state = dir.join("f.bin.idma-state");
    let size = 32 * MB;
    // 4 connections at 1 MiB/s: about 8 s in total. Checkpoints happen every 2 s.
    let url = format!("{base}/file/f.bin?size={size}&seed=5&rate=1M");

    let mut child = dm(&url, &out).spawn().expect("spawn dm");
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "dm finished before it could be killed"
    );
    child.kill().expect("kill");
    child.wait().expect("wait");
    assert!(state.exists(), "no state file after 5 s");
    assert!(!out.exists());

    let status = tokio::task::spawn_blocking(move || dm(&url, &out).status())
        .await
        .expect("join")
        .expect("run dm");
    assert!(status.success(), "{status}");
    let out = dir.join("f.bin");
    assert_eq!(verify_file(&out, 5, size).expect("verify"), Verdict::Ok);
    assert!(!state.exists());

    // Starting over would have sent the ~20 MiB of the first run again; resuming loses at
    // most the progress since the last checkpoint.
    let sent = bytes_sent(&base).await;
    assert!(
        sent < size + 14 * MB,
        "server sent {} MiB in total",
        sent / MB
    );
}

async fn bytes_sent(base: &str) -> u64 {
    let stats: serde_json::Value = reqwest::get(format!("{base}/_stats"))
        .await
        .expect("stats")
        .json()
        .await
        .expect("stats json");
    stats
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|u| u["bytes_sent"].as_u64())
        .sum()
}
