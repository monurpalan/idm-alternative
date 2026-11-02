//! `shard` killed mid-download (TerminateProcess / SIGKILL, no chance to clean up) resumes
//! from its state file when run again, and the result is byte-identical.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use shardload_testserver::{Verdict, router, verify_file};

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
        .join("shardload-cli-tests")
        .join(format!("kill-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create dir");
    let out = dir.join("f.bin");
    let state = dir.join("f.bin.shardload-state");
    let size = 32 * MB;
    // 4 connections at 1 MiB/s: about 8 s in total. Checkpoints happen every 2 s.
    let url = format!("{base}/file/f.bin?size={size}&seed=5&rate=1M");

    let mut child = shard(&url, &out).spawn().expect("spawn shard");
    // Kill once a checkpoint has recorded 20 MiB, not after a fixed time: on a slow CI machine
    // the first checkpoints can come late.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while saved(&state) < 20 * MB {
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "shard finished before it could be killed"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "no checkpoint of 20 MiB within 60 s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    child.kill().expect("kill");
    child.wait().expect("wait");
    assert!(state.exists());
    assert!(!out.exists());
    // What the resumed run must fetch, from the state file as the kill left it.
    let expected = to_fetch(&state);
    // The killed connections' last writes counted before the count is taken.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while active(&base).await > 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the server still sends"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let before = bytes_sent(&base).await;

    let status = tokio::task::spawn_blocking(move || shard(&url, &out).status())
        .await
        .expect("join")
        .expect("run shard");
    assert!(status.success(), "{status}");
    let out = dir.join("f.bin");
    assert_eq!(verify_file(&out, 5, size).expect("verify"), Verdict::Ok);
    assert!(!state.exists());

    // Starting over would fetch the whole file again. Resuming fetches what the state file
    // left out, and each part's last seconds; connections whose ranges move when parts are
    // split leave what their socket buffers hold, a little more.
    let again = bytes_sent(&base).await - before;
    assert!(
        (expected..expected + 2 * MB).contains(&again) && expected < size,
        "fetched {again} bytes again, {expected} expected, of {size}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Bytes a run resuming from `state` downloads: each part's rest, and its last seconds again
/// (the engine's `State::rewind`: 2 s at the part's speed, at least 1 MiB).
fn to_fetch(state: &std::path::Path) -> u64 {
    let text = std::fs::read_to_string(state).expect("state file");
    let state: serde_json::Value = serde_json::from_str(&text).expect("state json");
    let field = |s: &serde_json::Value, k: &str| s[k].as_u64().unwrap_or(0);
    state["segments"]
        .as_array()
        .expect("segments")
        .iter()
        .map(|s| {
            let back = (field(s, "speed") * 2).max(MB);
            let kept = field(s, "written")
                .saturating_sub(back)
                .max(field(s, "start"));
            field(s, "end") - kept
        })
        .sum()
}

/// Requests the server is still answering.
async fn active(base: &str) -> u64 {
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
        .filter_map(|u| u["active"].as_u64())
        .sum()
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
