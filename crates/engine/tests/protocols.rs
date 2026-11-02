//! Downloads over FTP, FTPS (explicit, `AUTH TLS`) and SFTP from real servers on this machine:
//! in parts, logging in, resuming, refusing what must be refused.

mod common;

use std::path::Path;

use common::*;
use shardload_engine::{CancellationToken, Error, Limiter, Options, Progress, download, probe};
use shardload_testserver::{ftp, sftp};

/// A folder for a server to serve, with one file of `size` bytes of seed `seed`.
fn served(dir: &WorkDir, size: u64, seed: u64) -> std::path::PathBuf {
    let root = dir.join("served");
    ftp::file(&root, "pub/file.bin", size, seed).expect("file");
    root
}

#[tokio::test(flavor = "multi_thread")]
async fn ftp_downloads_in_parts_and_logs_in() {
    let dir = workdir("ftp");
    let size = 12 * MB;
    let port = ftp::serve(served(&dir, size, 41), false)
        .await
        .expect("serve");
    // Anonymous, in parts (the server offers REST).
    let url = format!("ftp://127.0.0.1:{port}/pub/file.bin");
    let p = probe(&url, &[], &opts(4)).await.expect("probe");
    assert_eq!(
        (p.size, p.resumable, p.name.as_str()),
        (Some(size), true, "file.bin")
    );
    let out = dir.join("anon.bin");
    let (tx, mut rx) = tokio::sync::watch::channel(Progress::default());
    let peak = tokio::spawn(async move {
        let mut parts = 0;
        while rx.changed().await.is_ok() {
            parts = parts.max(rx.borrow().segments.len());
        }
        parts
    });
    download(
        &request(url, &out),
        &opts(4),
        Some(&tx),
        &CancellationToken::new(),
    )
    .await
    .expect("download");
    drop(tx);
    assert_verified(&out, 41, size);
    assert_eq!(
        peak.await.expect("join"),
        3,
        "12 MiB in parts of at least 4 MiB"
    );
    // With a user name and password, in the address or as a saved login sends them.
    let out = dir.join("user.bin");
    let url = format!(
        "ftp://{}:{}@127.0.0.1:{port}/pub/file.bin",
        ftp::USER,
        ftp::PASSWORD
    );
    get(&request(url, &out), &opts(4)).await.expect("as a user");
    assert_verified(&out, 41, size);
    let out = dir.join("saved.bin");
    let mut req = request(format!("ftp://127.0.0.1:{port}/pub/file.bin"), &out);
    req.headers = vec![shardload_engine::basic_auth(ftp::USER, ftp::PASSWORD)];
    get(&req, &opts(4)).await.expect("with a saved login");
    assert_verified(&out, 41, size);
    // A wrong password, and a file that is not there.
    let bad = format!("ftp://{}:wrong@127.0.0.1:{port}/pub/file.bin", ftp::USER);
    let e = get(&request(bad, &dir.join("bad.bin")), &opts(4)).await;
    assert!(matches!(e, Err(Error::AuthRequired { .. })), "{e:?}");
    let missing = format!("ftp://127.0.0.1:{port}/pub/none.bin");
    let e = get(&request(missing, &dir.join("none.bin")), &opts(4)).await;
    assert!(matches!(e, Err(Error::NotFound(_))), "{e:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ftp_resumes_where_it_stopped() {
    let dir = workdir("ftp-resume");
    let size = 16 * MB;
    let port = ftp::serve(served(&dir, size, 42), false)
        .await
        .expect("serve");
    let url = format!("ftp://127.0.0.1:{port}/pub/file.bin");
    let out = dir.join("f.bin");
    // Slowed down, and stopped once 4 MiB are in.
    let slow = Options {
        limiters: vec![Limiter::new(Some(4 * MB))],
        ..opts(4)
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
    assert!(Path::new(&format!("{}.shardload-state", out.display())).exists());
    let done = get(&request(url, &out), &opts(4)).await.expect("resume");
    assert!(
        done.resumed_from >= 4 * MB,
        "resumed from {}",
        done.resumed_from
    );
    assert_verified(&out, 42, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn ftps_checks_the_certificate() {
    let dir = workdir("ftps");
    let size = 6 * MB;
    let port = ftp::serve(served(&dir, size, 43), true)
        .await
        .expect("serve");
    let url = format!("ftpes://127.0.0.1:{port}/pub/file.bin");
    // A self-signed certificate is refused...
    let e = get(&request(url.clone(), &dir.join("refused.bin")), &opts(2)).await;
    assert!(
        matches!(&e, Err(Error::Remote(m)) if m.contains("certificate")),
        "{e:?}"
    );
    // ...unless invalid certificates are allowed.
    let out = dir.join("f.bin");
    let insecure = Options {
        insecure: true,
        ..opts(2)
    };
    get(&request(url, &out), &insecure).await.expect("over TLS");
    assert_verified(&out, 43, size);
}

#[tokio::test(flavor = "multi_thread")]
async fn sftp_downloads_and_trusts_a_host_key_once() {
    let dir = workdir("sftp");
    let size = 10 * MB;
    let root = served(&dir, size, 44);
    let server = sftp::serve(root.clone()).await.expect("serve");
    let known = dir.join("known_hosts");
    let options = Options {
        known_hosts: Some(known.clone()),
        ..opts(4)
    };
    let url = |port: u16, name: &str| {
        format!(
            "sftp://{}:{}@127.0.0.1:{port}/pub/{name}",
            ftp::USER,
            ftp::PASSWORD
        )
    };
    // Seen for the first time: recorded, and trusted from then on.
    let out = dir.join("f.bin");
    get(&request(url(server.port, "file.bin"), &out), &options)
        .await
        .expect("download");
    assert_verified(&out, 44, size);
    let recorded = std::fs::read_to_string(&known).expect("known hosts");
    assert!(
        recorded.contains(&format!("[127.0.0.1]:{}", server.port)),
        "{recorded}"
    );
    let again = dir.join("again.bin");
    get(&request(url(server.port, "file.bin"), &again), &options)
        .await
        .expect("again");
    assert_verified(&again, 44, size);
    // Without a file of its own, a server not in ~/.ssh/known_hosts is refused.
    let strict = Options {
        known_hosts: None,
        ..opts(4)
    };
    let e = get(
        &request(url(server.port, "file.bin"), &dir.join("s.bin")),
        &strict,
    )
    .await;
    assert!(
        matches!(&e, Err(Error::Remote(m)) if m.contains("not in")),
        "{e:?}"
    );
    // Another server answering where a known one was: its key changed, refused.
    let other = sftp::serve(root).await.expect("serve");
    russh::keys::known_hosts::learn_known_hosts_path(
        "127.0.0.1",
        other.port,
        &server.host_key,
        &known,
    )
    .expect("learn");
    let e = get(
        &request(url(other.port, "file.bin"), &dir.join("o.bin")),
        &options,
    )
    .await;
    assert!(
        matches!(&e, Err(Error::Remote(m)) if m.contains("changed")),
        "{e:?}"
    );
    // A wrong password, and a file that is not there.
    let bad = format!(
        "sftp://{}:wrong@127.0.0.1:{}/pub/file.bin",
        ftp::USER,
        server.port
    );
    let e = get(&request(bad, &dir.join("b.bin")), &options).await;
    assert!(matches!(e, Err(Error::AuthRequired { .. })), "{e:?}");
    let e = get(
        &request(url(server.port, "none.bin"), &dir.join("n.bin")),
        &options,
    )
    .await;
    assert!(matches!(e, Err(Error::NotFound(_))), "{e:?}");
}
