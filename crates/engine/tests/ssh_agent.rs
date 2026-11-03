//! SFTP logging in with a key held by the user's SSH agent (`SSH_AUTH_SOCK`), against the
//! OpenSSH server of `real_servers.rs`. A test binary of its own: it sets the agent for the
//! whole process. Unix only (OpenSSH's `ssh-agent`).

#![cfg(unix)]

mod common;

use std::path::PathBuf;
use std::process::Command;

use common::*;
use shardload_engine::{Error, Options};

#[test]
fn sftp_logs_in_with_the_agents_key() {
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
    // SAFETY: set before the runtime starts, in a test binary with this one test.
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &sock) };

    let options = Options {
        known_hosts: Some(dir.join("known_hosts")),
        ssh_dir: Some(keys),
        ..opts(2)
    };
    let url = "sftp://shard@127.0.0.1:9922/home/shard/pub/file.bin".to_owned();
    let out = dir.join("f.bin");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let done = runtime.block_on(get(&request(url.clone(), &out), &options));
    agent.kill().ok();
    agent.wait().ok();
    done.expect("with the agent's key");
    let len = std::fs::metadata(&out).expect("file").len();
    assert_eq!(len, 13_631_489);
    // Without the agent, the key in the folder is locked.
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let e = runtime.block_on(get(&request(url, &dir.join("g.bin")), &options));
    assert!(matches!(e, Err(Error::KeyLocked { .. })), "{e:?}");
}
