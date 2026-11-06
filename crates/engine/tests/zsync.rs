//! Delta downloads with zsync: a newer version of a file built from an older copy, only the
//! changed blocks downloaded; checked by its SHA-1, and downloaded whole when that fails.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use common::*;
use shardload_engine::CancellationToken;
use shardload_engine::zsync::{Control, update};

/// Files served from memory with ranges; the bytes sent are counted, the ranges asked for
/// kept, and each range answered after `delay_ms`.
#[derive(Default)]
struct Files {
    files: std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
    sent: AtomicU64,
    asked: std::sync::Mutex<Vec<(u64, u64)>>,
    delay_ms: AtomicU64,
}

/// Sent by the process that is killed part way (see
/// `a_killed_update_resumes_with_only_the_missing_ranges`).
const CHILD: &str = "x-killed-child";

async fn serve(
    State(s): State<Arc<Files>>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> impl IntoResponse {
    let Some(data) = s.files.lock().expect("lock").get(&name).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let range = h
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.split_once('-'))
        .and_then(|(a, b)| {
            let a: usize = a.parse().ok()?;
            let b = if b.is_empty() {
                data.len() - 1
            } else {
                b.parse::<usize>().ok()?
            };
            Some((a, b.min(data.len() - 1)))
        });
    let total = data.len();
    match range {
        Some((a, b)) if a <= b => {
            // A killed child's last request may only be handled after the parent has
            // cleared the list: it does not count.
            if !h.contains_key(CHILD) {
                s.asked.lock().expect("lock").push((a as u64, b as u64 + 1));
            }
            let delay = s.delay_ms.load(Ordering::Relaxed);
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            s.sent.fetch_add((b + 1 - a) as u64, Ordering::Relaxed);
            (
                StatusCode::PARTIAL_CONTENT,
                [
                    (header::CONTENT_RANGE, format!("bytes {a}-{b}/{total}")),
                    (header::ACCEPT_RANGES, "bytes".to_owned()),
                ],
                data[a..=b].to_vec(),
            )
                .into_response()
        }
        _ => {
            s.sent.fetch_add(total as u64, Ordering::Relaxed);
            ([(header::ACCEPT_RANGES, "bytes".to_owned())], data).into_response()
        }
    }
}

async fn server(files: Vec<(&str, Vec<u8>)>) -> (String, Arc<Files>) {
    let state = Arc::new(Files::default());
    state
        .files
        .lock()
        .expect("lock")
        .extend(files.into_iter().map(|(n, d)| (n.to_owned(), d)));
    let app = axum::Router::new()
        .route("/{name}", axum::routing::get(serve))
        .with_state(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move { axum::serve(listener, app).await });
    (base, state)
}

/// Version 2 of a file, and version 1: the same but for a part changed, bytes put in near
/// the start (so everything after moves) and an end cut short.
fn versions() -> (Vec<u8>, Vec<u8>) {
    let mut new = vec![0u8; (8 * MB) as usize];
    shardload_testserver::content::fill(61, 0, &mut new);
    let mut old = new.clone();
    old.truncate(old.len() - 50_000);
    old[(5 * MB) as usize..(5 * MB + 100_000) as usize].fill(7);
    old.splice(3 * MB as usize..3 * MB as usize, vec![1u8; 1000]);
    (new, old)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_version_is_built_from_the_old_copy() {
    let (new, old) = versions();
    let zsync = shardload_testserver::zsync::make(&new, "disk.iso", "disk.iso", 2048, 3, 5);
    let (base, served) = server(vec![
        ("disk.iso", new.clone()),
        ("disk.iso.zsync", zsync.clone()),
    ])
    .await;
    let dir = workdir("zsync");
    let seed = dir.join("disk-1.iso");
    std::fs::write(&seed, &old).expect("old copy");
    let control_url = format!("{base}/disk.iso.zsync");
    let control = shardload_engine::zsync::fetch_control(&control_url, &[], &opts(1))
        .await
        .expect("control");
    assert_eq!(control.filename.as_deref(), Some("disk.iso"));
    assert_eq!(control.target(&control_url), format!("{base}/disk.iso"));
    let sent_before = served.sent.load(Ordering::Relaxed);
    let out = dir.join("disk.iso");
    let delta = update(
        &control_url,
        &control,
        std::slice::from_ref(&seed),
        &request(String::new(), &out),
        &opts(1),
        None,
        &CancellationToken::new(),
    )
    .await
    .expect("update");
    assert_eq!(std::fs::read(&out).expect("file"), new);
    // The changed part, the blocks where bytes came in, and the end: a few hundred KiB.
    let fetched = served.sent.load(Ordering::Relaxed) - sent_before;
    assert!(fetched < MB / 2, "fetched {fetched}");
    assert_eq!(
        delta.reused + fetched - 1,
        new.len() as u64,
        "every byte either reused or fetched (+1 probe byte)"
    );
    assert_eq!(delta.from.as_deref(), Some(seed.as_path()));

    // Without an old copy every block is downloaded, still in ranges, still checked.
    let out = dir.join("fresh.iso");
    let delta = update(
        &control_url,
        &control,
        &[],
        &request(String::new(), &out),
        &opts(1),
        None,
        &CancellationToken::new(),
    )
    .await
    .expect("fresh");
    assert_eq!(delta.reused, 0);
    assert_eq!(std::fs::read(&out).expect("file"), new);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_match_falls_back_to_the_whole_file() {
    let (new, old) = versions();
    let mut zsync = shardload_testserver::zsync::make(&new, "disk.iso", "disk.iso", 2048, 3, 5);
    // A SHA-1 the result cannot have: as if a block had been matched by chance.
    let at = zsync.windows(6).position(|w| w == b"SHA-1:").expect("sha1") + 7;
    zsync[at] = if zsync[at] == b'0' { b'1' } else { b'0' };
    let (base, _) = server(vec![("disk.iso", new.clone())]).await;
    let dir = workdir("zsync-wrong");
    let seed = dir.join("old.iso");
    std::fs::write(&seed, &old).expect("old copy");
    let control = Control::parse(&zsync).expect("parse");
    let out = dir.join("disk.iso");
    let delta = update(
        &format!("{base}/disk.iso.zsync"),
        &control,
        &[seed],
        &request(String::new(), &out),
        &opts(2),
        None,
        &CancellationToken::new(),
    )
    .await
    .expect("whole file");
    assert_eq!(delta.reused, 0);
    assert_eq!(std::fs::read(&out).expect("file"), new);
}

/// Run as a child of [`a_killed_update_resumes_with_only_the_missing_ranges`]: the update it
/// gets killed in.
async fn child_update(control_url: &str) {
    let control = shardload_engine::zsync::fetch_control(control_url, &[], &opts(1))
        .await
        .expect("control");
    let seed = std::env::var_os("ZSYNC_KILL_SEED").expect("seed");
    let out = std::env::var_os("ZSYNC_KILL_OUT").expect("out");
    let mut req = request(String::new(), std::path::Path::new(&out));
    req.headers.push((CHILD.to_owned(), "1".to_owned()));
    let done = update(
        control_url,
        &control,
        &[seed.into()],
        &req,
        &opts(1),
        None,
        &CancellationToken::new(),
    )
    .await;
    panic!("not killed: {done:?}");
}

/// Killed part way (the process, not a cancel), an update resumes from its state file: only
/// the ranges it had not saved are asked for, the copies are not matched again, and the
/// file comes out the same.
#[tokio::test(flavor = "multi_thread")]
async fn a_killed_update_resumes_with_only_the_missing_ranges() {
    if let Some(url) = std::env::var_os("ZSYNC_KILL_URL") {
        return child_update(&url.to_string_lossy()).await;
    }
    // 16 MiB, of which the old copy lacks 12 in the middle: four ranges of 4 MiB at most.
    let mut new = vec![0u8; (16 * MB) as usize];
    shardload_testserver::content::fill(62, 0, &mut new);
    let mut old = new.clone();
    old[(2 * MB) as usize..(14 * MB) as usize].fill(7);
    let zsync = shardload_testserver::zsync::make(&new, "disk.iso", "disk.iso", 4096, 4, 8);
    let (base, served) = server(vec![
        ("disk.iso", new.clone()),
        ("disk.iso.zsync", zsync.clone()),
    ])
    .await;
    served.delay_ms.store(700, Ordering::Relaxed);
    let dir = workdir("zsync-kill");
    let seed = dir.join("old.iso");
    std::fs::write(&seed, &old).expect("old copy");
    let out = dir.join("disk.iso");
    let state_path = dir.join("disk.iso.shardload-state");
    let control_url = format!("{base}/disk.iso.zsync");
    let mut child = std::process::Command::new(std::env::current_exe().expect("exe"))
        .args([
            "--exact",
            "a_killed_update_resumes_with_only_the_missing_ranges",
            "--nocapture",
        ])
        .env("ZSYNC_KILL_URL", &control_url)
        .env("ZSYNC_KILL_SEED", &seed)
        .env("ZSYNC_KILL_OUT", &out)
        .spawn()
        .expect("child");
    // Killed once it has saved a range or two.
    let left = |state: &serde_json::Value| -> Vec<(u64, u64)> {
        state["segments"]
            .as_array()
            .expect("segments")
            .iter()
            .map(|s| {
                (
                    s["written"].as_u64().expect("w"),
                    s["end"].as_u64().expect("e"),
                )
            })
            .filter(|(w, e)| w < e)
            .collect()
    };
    let read = || -> Option<serde_json::Value> {
        serde_json::from_slice(&std::fs::read(&state_path).ok()?).ok()
    };
    let deadline = std::time::Instant::now() + LIMIT;
    let total = loop {
        assert!(std::time::Instant::now() < deadline, "no progress saved");
        assert!(
            child.try_wait().expect("child").is_none(),
            "the child ended"
        );
        if let Some(state) = read() {
            let segments = state["segments"].as_array().expect("segments");
            let all: u64 = segments
                .iter()
                .map(|s| s["end"].as_u64().expect("e") - s["start"].as_u64().expect("s"))
                .sum();
            let missing: u64 = left(&state).iter().map(|(w, e)| e - w).sum();
            if missing < all {
                break all;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    child.kill().expect("kill");
    child.wait().expect("wait");
    let state = read().expect("state after the kill");
    let missing = left(&state);
    let missing_bytes: u64 = missing.iter().map(|(w, e)| e - w).sum();
    assert!(missing_bytes > 0 && missing_bytes < total, "{missing:?}");
    assert!(shardload_engine::zsync::resumes(&out));

    served.delay_ms.store(0, Ordering::Relaxed);
    served.asked.lock().expect("lock").clear();
    let control = Control::parse(&zsync).expect("parse");
    // No copy this time: had it been matched again, its blocks would not be found.
    let delta = update(
        &control_url,
        &control,
        &[],
        &request(String::new(), &out),
        &opts(1),
        None,
        &CancellationToken::new(),
    )
    .await
    .expect("resume");
    assert_eq!(std::fs::read(&out).expect("file"), new);
    assert!(!state_path.exists());
    assert_eq!(delta.outcome.resumed_from, new.len() as u64 - missing_bytes);
    assert!(delta.reused > 0, "the copy's part counts as reused");
    // Besides the one-byte probe, exactly the ranges it had not saved.
    let asked: Vec<(u64, u64)> = served
        .asked
        .lock()
        .expect("lock")
        .iter()
        .copied()
        .filter(|&(a, b)| (a, b) != (0, 1))
        .collect();
    let asked_bytes: u64 = asked.iter().map(|(a, b)| b - a).sum();
    assert_eq!(
        asked_bytes, missing_bytes,
        "asked {asked:?}, missing {missing:?}"
    );
    assert!(
        asked
            .iter()
            .all(|&(a, b)| missing.iter().any(|&(w, e)| w <= a && b <= e)),
        "asked {asked:?}, missing {missing:?}"
    );
}

#[test]
fn compressed_and_cut_short_files_are_refused() {
    let new = vec![5u8; 10_000];
    let good = shardload_testserver::zsync::make(&new, "a", "a", 1024, 2, 4);
    assert!(Control::parse(&good).is_ok());
    assert!(Control::parse(&good[..good.len() - 1]).is_err());
    let mut gz = b"zsync: 0.6.2\nZ-Map2: 3\n".to_vec();
    gz.extend_from_slice(&good["zsync: 0.6.2\n".len()..]);
    assert!(Control::parse(&gz).is_err());
    assert!(Control::parse(b"<html>not found</html>").is_err());
}

/// With zsyncmake on PATH (CI installs it on Linux), a `.zsync` it made is read the same way.
#[tokio::test(flavor = "multi_thread")]
async fn files_made_by_zsyncmake_work() {
    let Ok(_) = std::process::Command::new("zsyncmake").arg("-V").output() else {
        // CI installs it on Linux: there it must not be skipped.
        assert!(
            !(cfg!(target_os = "linux") && std::env::var_os("CI").is_some()),
            "zsyncmake missing on CI"
        );
        eprintln!("zsyncmake not found: skipped");
        return;
    };
    let (new, old) = versions();
    let dir = workdir("zsyncmake");
    let file = dir.join("disk.iso");
    std::fs::write(&file, &new).expect("file");
    let made = dir.join("disk.iso.zsync");
    let status = std::process::Command::new("zsyncmake")
        .args(["-u", "disk.iso", "-o"])
        .arg(&made)
        .arg(&file)
        .status()
        .expect("zsyncmake");
    assert!(status.success());
    let zsync = std::fs::read(&made).expect("zsync");
    let (base, served) = server(vec![("disk.iso", new.clone())]).await;
    let seed = dir.join("old.iso");
    std::fs::write(&seed, &old).expect("old copy");
    let control = Control::parse(&zsync).expect("parse");
    let out = dir.join("out.iso");
    let delta = update(
        &format!("{base}/disk.iso.zsync"),
        &control,
        &[seed],
        &request(String::new(), &out),
        &opts(1),
        None,
        &CancellationToken::new(),
    )
    .await
    .expect("update");
    assert_eq!(std::fs::read(&out).expect("file"), new);
    assert!(
        delta.reused > new.len() as u64 * 9 / 10,
        "reused {}",
        delta.reused
    );
    assert!(served.sent.load(Ordering::Relaxed) < MB, "fetched too much");
}
