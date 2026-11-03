//! SFTP logging in with a key held by the user's SSH agent (`SSH_AUTH_SOCK`), against the
//! OpenSSH server of `real_servers.rs`. The downloads run in a child process of this test,
//! which is given the agent. Unix only (OpenSSH's `ssh-agent`).

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use shardload_engine::{Error, Options};

const NAME: &str = "sftp_logs_in_with_the_agents_key";

/// The child: downloads with the keys in `SSH_KEYS_DIR`, and the agent if it was given one.
fn child(keys: &Path) {
    let dir = keys.parent().expect("dir");
    let options = Options {
        known_hosts: Some(dir.join("known_hosts")),
        ssh_dir: Some(keys.to_owned()),
        ..opts(2)
    };
    let url = "sftp://shard@127.0.0.1:9922/home/shard/pub/file.bin".to_owned();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let out = dir.join(format!("{}.bin", std::process::id()));
    let done = runtime.block_on(get(&request(url, &out), &options));
    if std::env::var_os("SSH_AUTH_SOCK").is_some() {
        done.expect("with the agent's key");
        assert_eq!(std::fs::metadata(&out).expect("file").len(), 13_631_489);
    } else {
        // Without the agent, the key in the folder is locked.
        assert!(matches!(done, Err(Error::KeyLocked { .. })), "{done:?}");
    }
}

#[test]
fn sftp_logs_in_with_the_agents_key() {
    if let Some(keys) = std::env::var_os("SSH_KEYS_DIR") {
        return child(Path::new(&keys));
    }
    let Some(servers) = std::env::var_os("SHARDLOAD_REAL_SERVERS").map(PathBuf::from) else {
        assert!(
            std::env::var("GITHUB_JOB").as_deref() != Ok("linux"),
            "SHARDLOAD_REAL_SERVERS missing on CI"
        );
        eprintln!("SHARDLOAD_REAL_SERVERS not set: skipped");
        return;
    };
    let dir = workdir("ssh-agent");
    // The key with a passphrase is in the folder, and its twin without one in the agent:
    // what logs in is the agent.
    let keys = dir.join("keys");
    std::fs::create_dir_all(&keys).expect("keys");
    std::fs::copy(servers.join("id_locked"), keys.join("id_ed25519")).expect("key");
    let open = dir.join("open");
    std::fs::copy(servers.join("id_locked"), &open).expect("key");
    std::fs::set_permissions(&open, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .expect("chmod");
    let changed = Command::new("ssh-keygen")
        .args(["-q", "-p", "-P", "key-pass", "-N", "", "-f"])
        .arg(&open)
        .status()
        .expect("ssh-keygen");
    assert!(changed.success());
    let sock = dir.join("agent.sock");
    let mut agent = Command::new("ssh-agent")
        .arg("-D")
        .arg("-a")
        .arg(&sock)
        .spawn()
        .expect("ssh-agent");
    for _ in 0..100 {
        if sock.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let added = Command::new("ssh-add")
        .arg(&open)
        .env("SSH_AUTH_SOCK", &sock)
        .status()
        .expect("ssh-add");
    assert!(added.success());
    let run = |agent: Option<&Path>| {
        let mut child = Command::new(std::env::current_exe().expect("exe"));
        child
            .args(["--exact", NAME, "--nocapture"])
            .env("SSH_KEYS_DIR", &keys);
        match agent {
            Some(sock) => child.env("SSH_AUTH_SOCK", sock),
            None => child.env_remove("SSH_AUTH_SOCK"),
        };
        child.status().expect("child").success()
    };
    let with = run(Some(&sock));
    let without = run(None);
    agent.kill().ok();
    agent.wait().ok();
    assert!(with, "with the agent");
    assert!(without, "without the agent");
}
