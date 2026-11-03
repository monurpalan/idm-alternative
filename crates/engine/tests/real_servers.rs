//! FTPS and SFTP against real servers rather than the test server's: vsftpd (implicit TLS on
//! 9990, explicit `AUTH TLS` on 9921) and OpenSSH (9922), started by
//! `scripts/real-servers/setup.sh` (CI, on the runner) or `run.sh` (in a container). The
//! folder it fills is given as `SHARDLOAD_REAL_SERVERS`; without it these tests are skipped,
//! except in CI's `linux` job, which starts the servers: there they must run.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use sha2::{Digest, Sha256};
use shardload_engine::{CancellationToken, Error, Limiter, Options, Progress, download, probe};

const USER: &str = "shard";
const PASSWORD: &str = "shard-pass";
/// pub/file.bin in the user's home.
const SIZE: u64 = 13_631_489;

/// What setup.sh wrote, or `None` to skip.
fn servers() -> Option<PathBuf> {
    let dir = std::env::var_os("SHARDLOAD_REAL_SERVERS").map(PathBuf::from);
    if dir.is_none() {
        assert!(
            std::env::var("GITHUB_JOB").as_deref() != Ok("linux"),
            "SHARDLOAD_REAL_SERVERS missing on CI"
        );
        eprintln!("SHARDLOAD_REAL_SERVERS not set: skipped");
    }
    dir
}

fn assert_same(servers: &Path, file: &Path) {
    let want = std::fs::read_to_string(servers.join("file.sha256")).expect("sha256");
    let got = Sha256::digest(std::fs::read(file).expect("downloaded"));
    let got: String = got.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(got, want.trim(), "{}", file.display());
    assert!(!PathBuf::from(format!("{}.shardload-state", file.display())).exists());
}

fn insecure(connections: usize) -> Options {
    Options {
        insecure: true,
        ..opts(connections)
    }
}

/// FTPS with implicit TLS (`ftps://`) and explicit (`ftpes://`): in parts, the self-signed
/// certificate refused unless allowed, a wrong password, a file that is not there.
#[tokio::test(flavor = "multi_thread")]
async fn ftps_implicit_and_explicit() {
    let Some(servers) = servers() else { return };
    let dir = workdir("real-ftps");
    for (scheme, port) in [("ftps", 9990), ("ftpes", 9921)] {
        let url = format!("{scheme}://{USER}:{PASSWORD}@127.0.0.1:{port}/pub/file.bin");
        let p = probe(&url, &[], &insecure(4)).await.expect("probe");
        assert_eq!((p.size, p.resumable), (Some(SIZE), true), "{scheme}");
        let out = dir.join(&format!("{scheme}.bin"));
        let (tx, mut rx) = tokio::sync::watch::channel(Progress::default());
        let peak = tokio::spawn(async move {
            let mut parts = 0;
            while rx.changed().await.is_ok() {
                parts = parts.max(rx.borrow().segments.len());
            }
            parts
        });
        download(
            &request(url.clone(), &out),
            &insecure(4),
            Some(&tx),
            &CancellationToken::new(),
        )
        .await
        .expect(scheme);
        drop(tx);
        assert_same(&servers, &out);
        assert_eq!(peak.await.expect("join"), 3, "{scheme}: in parts");

        let e = get(&request(url, &dir.join("refused.bin")), &opts(2)).await;
        assert!(
            matches!(&e, Err(Error::Remote(m)) if m.contains("certificate")),
            "{scheme}: {e:?}"
        );
        let bad = format!("{scheme}://{USER}:wrong@127.0.0.1:{port}/pub/file.bin");
        let e = get(&request(bad, &dir.join("bad.bin")), &insecure(2)).await;
        assert!(
            matches!(e, Err(Error::AuthRequired { .. })),
            "{scheme}: {e:?}"
        );
        let missing = format!("{scheme}://{USER}:{PASSWORD}@127.0.0.1:{port}/pub/none.bin");
        let e = get(&request(missing, &dir.join("none.bin")), &insecure(2)).await;
        assert!(matches!(e, Err(Error::NotFound(_))), "{scheme}: {e:?}");
    }
}

/// Stopped once 4 MiB are in, implicit FTPS resumes from where it stopped.
#[tokio::test(flavor = "multi_thread")]
async fn ftps_resumes() {
    let Some(servers) = servers() else { return };
    let dir = workdir("real-ftps-resume");
    let url = format!("ftps://{USER}:{PASSWORD}@127.0.0.1:9990/pub/file.bin");
    let out = dir.join("f.bin");
    let slow = Options {
        limiters: vec![Limiter::new(Some(4 * MB))],
        ..insecure(4)
    };
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::watch::channel(Progress::default());
    let stop = cancel.clone();
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            if rx.borrow().received >= 4 * MB {
                stop.cancel();
            }
        }
    });
    let first = download(&request(url.clone(), &out), &slow, Some(&tx), &cancel).await;
    assert!(matches!(first, Err(Error::Cancelled)), "{first:?}");
    let done = get(&request(url, &out), &insecure(4))
        .await
        .expect("resume");
    assert!(
        done.resumed_from >= 4 * MB,
        "resumed from {}",
        done.resumed_from
    );
    assert_same(&servers, &out);
}

/// A folder of SSH keys holding `key` (from setup.sh) as `id_ed25519`.
fn ssh_dir(servers: &Path, dir: &WorkDir, key: &str) -> PathBuf {
    let keys = dir.join(&format!("keys-{key}"));
    std::fs::create_dir_all(&keys).expect("keys");
    std::fs::copy(servers.join(key), keys.join("id_ed25519")).expect("key");
    keys
}

/// SFTP from OpenSSH with the user's key: without a passphrase; with one, opened by the
/// password given (and a clear error without it, or with a wrong one); or with the password.
#[tokio::test(flavor = "multi_thread")]
async fn sftp_keys_with_and_without_a_passphrase() {
    let Some(servers) = servers() else { return };
    let dir = workdir("real-sftp");
    let known = dir.join("known_hosts");
    let options = |key: &str| Options {
        known_hosts: Some(known.clone()),
        ssh_dir: Some(ssh_dir(&servers, &dir, key)),
        ..opts(4)
    };
    let url = |login: &str| format!("sftp://{login}@127.0.0.1:9922/home/shard/pub/file.bin");

    let out = dir.join("plain.bin");
    let p = probe(&url(USER), &[], &options("id_plain"))
        .await
        .expect("probe");
    assert_eq!((p.size, p.resumable), (Some(SIZE), true));
    get(&request(url(USER), &out), &options("id_plain"))
        .await
        .expect("with a key without a passphrase");
    assert_same(&servers, &out);

    let locked = options("id_locked");
    let e = get(&request(url(USER), &dir.join("none.bin")), &locked).await;
    assert!(
        matches!(&e, Err(Error::KeyLocked { key, wrong: false }) if key.ends_with("id_ed25519")),
        "{e:?}"
    );
    let e = get(
        &request(url(&format!("{USER}:nope")), &dir.join("wrong.bin")),
        &locked,
    )
    .await;
    assert!(
        matches!(&e, Err(Error::KeyLocked { wrong: true, .. })),
        "{e:?}"
    );
    let message = e.expect_err("wrong").to_string();
    assert!(
        message.contains("passphrase") && message.contains("not it"),
        "{message}"
    );

    let out = dir.join("locked.bin");
    get(&request(url(&format!("{USER}:key-pass")), &out), &locked)
        .await
        .expect("with the passphrase");
    assert_same(&servers, &out);
    // As a saved login sends it.
    let out = dir.join("saved.bin");
    let mut req = request(url(USER), &out);
    req.headers = vec![shardload_engine::basic_auth(USER, "key-pass")];
    get(&req, &locked).await.expect("with a saved passphrase");
    assert_same(&servers, &out);
    // The account's password still logs in, the key unopened.
    let out = dir.join("password.bin");
    get(&request(url(&format!("{USER}:{PASSWORD}")), &out), &locked)
        .await
        .expect("with the password");
    assert_same(&servers, &out);
}
