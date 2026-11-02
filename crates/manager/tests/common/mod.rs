//! Helpers shared by the manager's integration tests against the test server.

#![allow(dead_code)] // each test binary uses a different subset

use std::path::{Path, PathBuf};
use std::time::Duration;

use shardload_manager::{Config, Manager, NewTask, Status, Task, TaskId};
use shardload_testserver::{Verdict, router, verify_file};

pub const MB: u64 = 1 << 20;

pub async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, router()).await });
    format!("http://{addr}")
}

/// A fresh folder for one test, removed when the test ends.
pub struct WorkDir(pub PathBuf);

impl WorkDir {
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir()
            .join("shardload-manager-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create workdir");
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn db(&self) -> PathBuf {
        self.0.join("list.db")
    }

    /// Without the search for published checksums: its requests would count in the bytes
    /// the server sent.
    pub fn config(&self, max_active: usize) -> Config {
        Config {
            dir: self.0.join("downloads"),
            max_active,
            connections: 4,
            engine: shardload_engine::Options {
                find_checksum: false,
                ..shardload_engine::Options::default()
            },
            ..Config::default()
        }
    }

    /// One download at a time over one connection: its requests come in a known order.
    pub fn one_connection(&self) -> Config {
        Config {
            connections: 1,
            ..self.config(1)
        }
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

pub fn new_task(url: String) -> NewTask {
    NewTask {
        url,
        start: true,
        ..NewTask::default()
    }
}

/// Polls until `pred` holds for the task, or panics after `secs`.
pub async fn wait_for(m: &Manager, id: TaskId, secs: u64, pred: impl Fn(&Task) -> bool) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let t = m.get(id).await.expect("get");
        if pred(&t) {
            return t;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out; task is {:?} ({:?})",
            t.status,
            t.error
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub async fn wait_done(m: &Manager, id: TaskId) -> Task {
    let t = wait_for(m, id, 60, |t| {
        matches!(t.status, Status::Done | Status::Error)
    })
    .await;
    assert_eq!(t.status, Status::Done, "{:?}", t.error);
    t
}

pub fn assert_file(t: &Task, seed: u64, size: u64) {
    let path = t.path.as_ref().expect("path");
    assert_eq!(verify_file(path, seed, size).expect("verify"), Verdict::Ok);
}

pub async fn bytes_sent(base: &str) -> u64 {
    let stats: Vec<serde_json::Value> = reqwest::get(format!("{base}/_stats"))
        .await
        .expect("stats")
        .json()
        .await
        .expect("stats json");
    stats.iter().filter_map(|u| u["bytes_sent"].as_u64()).sum()
}

/// Every GET's `Range` header so far, for all URLs: empty for none.
pub async fn ranges_asked(base: &str) -> Vec<String> {
    let stats: Vec<serde_json::Value> = reqwest::get(format!("{base}/_stats"))
        .await
        .expect("stats")
        .json()
        .await
        .expect("stats json");
    stats
        .iter()
        .flat_map(|u| u["ranges"].as_array().cloned().unwrap_or_default())
        .map(|r| r.as_str().unwrap_or_default().to_owned())
        .collect()
}
