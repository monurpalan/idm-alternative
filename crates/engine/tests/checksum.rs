//! Verifying the finished file against a checksum the user supplied.

mod common;

use idma_engine::{CancellationToken, Checksum, Error, Phase, Progress, download};
use idma_testserver::content::fill;
use sha2::{Digest, Sha256};

use common::{LIMIT, MB, assert_verified, get, opts, request, start, workdir};

/// SHA-256 of the test server's content for `seed` and `size`.
fn sha256_of(seed: u64, size: u64) -> String {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; MB as usize];
    let mut offset = 0;
    while offset < size {
        let n = (size - offset).min(MB) as usize;
        fill(seed, offset, &mut buf[..n]);
        h.update(&buf[..n]);
        offset += n as u64;
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

#[tokio::test(flavor = "multi_thread")]
async fn matching_checksum_passes_and_reports_verifying() {
    let base = start().await;
    let dir = workdir("checksum-ok");
    let out = dir.join("f.bin");
    let size = 8 * MB;
    let req = request(format!("{base}/file/f.bin?size={size}&seed=11"), &out);
    let mut o = opts(4);
    o.checksum = Some(sha256_of(11, size).parse().expect("checksum"));

    let (tx, mut rx) = tokio::sync::watch::channel(Progress::default());
    let watcher = tokio::spawn(async move {
        let mut phases = Vec::new();
        while rx.changed().await.is_ok() {
            let phase = rx.borrow_and_update().phase;
            if phases.last() != Some(&phase) {
                phases.push(phase);
            }
        }
        phases
    });
    let done = tokio::time::timeout(
        LIMIT,
        download(&req, &o, Some(&tx), &CancellationToken::new()),
    )
    .await
    .expect("hung")
    .expect("download");
    drop(tx);
    assert_eq!(done.size, size);
    assert_verified(&out, 11, size);
    let phases = watcher.await.expect("join");
    assert!(phases.contains(&Phase::Verifying), "{phases:?}");
    assert_eq!(phases.last(), Some(&Phase::Done), "{phases:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn mismatch_fails_and_keeps_the_file() {
    let base = start().await;
    let dir = workdir("checksum-bad");
    let out = dir.join("f.bin");
    let size = 2 * MB;
    let req = request(format!("{base}/file/f.bin?size={size}&seed=12"), &out);
    let mut o = opts(4);
    let wrong: Checksum = sha256_of(13, size).parse().expect("checksum");
    o.checksum = Some(wrong.clone());
    match get(&req, &o).await {
        Err(Error::ChecksumMismatch { expected, actual }) => {
            assert_eq!(expected, wrong.to_string());
            assert_eq!(actual, sha256_of(12, size));
        }
        other => panic!("expected a mismatch, got {other:?}"),
    }
    // The file is there for inspection; nothing is left to resume.
    assert_verified(&out, 12, size);
    assert!(!dir.join("f.bin.idma-state").exists());
    assert!(!dir.join("f.bin.idma-part").exists());
}
