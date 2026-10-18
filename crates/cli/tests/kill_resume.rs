//! `shard` killed mid-download (TerminateProcess / SIGKILL, no chance to clean up) resumes
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

fn shard(url: &str, out: &PathBuf) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_shard"));
    c.args(["get", url, "-o"])
        .arg(out)
        // Requests for checksum files would count in the bytes the server sent.
        .args(["-c", "4", "-q", "--no-checksum-search"])
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

    let mut child = shard(&url, &out).spawn().expect("spawn shard");
    // Kill once a checkpoint has recorded 8 MiB, not after a fixed time: on a slow CI machine
    // the first checkpoints can come late.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while saved(&state) < 8 * MB {
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "shard finished before it could be killed"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "no checkpoint of 8 MiB within 60 s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    child.kill().expect("kill");
    child.wait().expect("wait");
    assert!(state.exists());
    assert!(!out.exists());
    let before = bytes_sent(&base).await;

    let status = tokio::task::spawn_blocking(move || shard(&url, &out).status())
        .await
        .expect("join")
        .expect("run shard");
    assert!(status.success(), "{status}");
    let out = dir.join("f.bin");
    assert_eq!(verify_file(&out, 5, size).expect("verify"), Verdict::Ok);
    assert!(!state.exists());

    // Starting over would fetch the whole file again. Resuming keeps what the last
    // checkpoint recorded, at least the 8 MiB waited for above.
    let again = bytes_sent(&base).await - before;
    assert!(
        again < size - 4 * MB,
        "fetched {} of {} MiB again",
        again / MB,
        size / MB
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Bytes the state file records as written, 0 while there is none.
fn saved(state: &std::path::Path) -> u64 {
    let Ok(text) = std::fs::read_to_string(state) else {
        return 0;
    };
    let Ok(state) = serde_json::from_str::<serde_json::Value>(&text) else {
        return 0;
    };
    state["segments"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| s["written"].as_u64().unwrap_or(0) - s["start"].as_u64().unwrap_or(0))
        .sum()
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
