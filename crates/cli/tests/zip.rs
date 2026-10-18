//! `shard zip list` and `shard zip get`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use shardload_testserver::router;
use shardload_testserver::zip::{sample, sample_entries};

async fn start() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, router()).await });
    format!("http://{addr}")
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("shardload-cli-tests")
        .join(format!("{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create dir");
    dir
}

async fn shard(args: Vec<String>) -> Output {
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_shard"))
            .args(&args)
            .args(["--browser", "none"])
            .output()
    })
    .await
    .expect("join")
    .expect("run shard")
}

fn args(list: &[&str], dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = list.iter().map(|s| (*s).to_owned()).collect();
    v.push("-d".into());
    v.push(dir.display().to_string());
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_and_picks_files() {
    let base = start().await;
    let url = format!("{base}/zip/sample.zip?seed=7");
    let out = shard(vec!["zip".into(), "list".into(), url.clone()]).await;
    assert!(out.status.success(), "{out:?}");
    let listing = String::from_utf8_lossy(&out.stdout);
    for e in sample_entries(7) {
        assert!(
            listing.contains(&e.name),
            "{} missing from\n{listing}",
            e.name
        );
    }
    assert!(listing.contains("(encrypted)"));

    let dir = fresh_dir("zip-get");
    let out = shard(args(
        &["zip", "get", &url, "data/raw.bin", "docs/*.txt", "-q"],
        &dir,
    ))
    .await;
    assert!(out.status.success(), "{out:?}");
    for (name, path) in [
        ("data/raw.bin", dir.join("data").join("raw.bin")),
        ("docs/notes.txt", dir.join("docs").join("notes.txt")),
    ] {
        let want = sample_entries(7)
            .into_iter()
            .find(|e| e.name == name)
            .expect("entry");
        assert!(std::fs::read(&path).expect("read") == want.data, "{name}");
    }
    assert!(!dir.join("readme.txt").exists());

    // A name that is not there: nothing is downloaded.
    let out = shard(args(&["zip", "get", &url, "missing.txt"], &dir)).await;
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing.txt"));

    // Entries that cannot be extracted fail the command, the rest is saved.
    let out = shard(args(
        &["zip", "get", &url, "secret.txt", "readme.txt"],
        &dir,
    ))
    .await;
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("encrypted"));
    assert!(dir.join("readme.txt").is_file());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn without_ranges_downloads_the_whole_archive() {
    let base = start().await;
    let url = format!("{base}/zip/sample.zip?seed=8&ranges=0");
    let dir = fresh_dir("zip-whole");
    let out = shard(args(&["zip", "get", &url, "readme.txt", "-q"], &dir)).await;
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("range requests"));
    let whole = std::fs::read(dir.join("sample.zip")).expect("archive");
    assert!(whole == sample(8, false));

    let out = shard(vec!["zip".into(), "list".into(), url]).await;
    assert!(!out.status.success());
    std::fs::remove_dir_all(&dir).ok();
}
