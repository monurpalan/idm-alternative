//! Helpers shared by the engine's integration tests.

#![allow(dead_code)] // each test binary uses a different subset

use std::path::{Path, PathBuf};
use std::time::Duration;

use idma_engine::{CancellationToken, Options, Outcome, Request, download};
use idma_testserver::{Verdict, router, verify_file};

pub const MB: u64 = 1 << 20;
/// No test may hang the suite.
pub const LIMIT: Duration = Duration::from_secs(60);

pub async fn start() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, router()).await });
    format!("http://{addr}")
}

/// A fresh, empty directory for one test, removed again when the test ends (downloads add
/// up to gigabytes per run). Keep it in a variable: a temporary is dropped at once.
pub struct WorkDir(PathBuf);

impl WorkDir {
    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

pub fn workdir(name: &str) -> WorkDir {
    let dir = std::env::temp_dir()
        .join("idma-engine-tests")
        .join(format!("{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create workdir");
    WorkDir(dir)
}

pub fn request(url: String, output: &Path) -> Request {
    Request {
        url,
        output: output.to_owned(),
        headers: Vec::new(),
        mirrors: Vec::new(),
    }
}

pub fn opts(connections: usize) -> Options {
    Options {
        connections,
        ..Options::default()
    }
}

pub async fn get(req: &Request, opts: &Options) -> idma_engine::Result<Outcome> {
    tokio::time::timeout(LIMIT, download(req, opts, None, &CancellationToken::new()))
        .await
        .expect("download hung")
}

pub fn assert_verified(path: &Path, seed: u64, size: u64) {
    assert_eq!(verify_file(path, seed, size).expect("verify"), Verdict::Ok);
    assert!(!PathBuf::from(format!("{}.idma-part", path.display())).exists());
    assert!(!PathBuf::from(format!("{}.idma-state", path.display())).exists());
}

/// Bytes the server sent for every URL so far.
pub async fn bytes_sent(base: &str) -> u64 {
    let text = reqwest::get(format!("{base}/_stats"))
        .await
        .expect("stats")
        .text()
        .await
        .expect("stats body");
    let stats: Vec<serde_json::Value> = serde_json::from_str(&text).expect("stats json");
    stats.iter().filter_map(|u| u["bytes_sent"].as_u64()).sum()
}

/// Tests that measure time take this lock so they do not compete with each other for the
/// CPU; on a small CI machine the competition alone can halve their speed.
pub async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}
