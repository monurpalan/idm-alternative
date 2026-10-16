//! The bridge between the browser and the app.
//!
//! ```text
//! extension ──native messaging (stdin/stdout)──▶ idma-host ──named pipe──▶ app
//! ```
//!
//! Both hops carry the same frames: a 4-byte little-endian length and that many bytes of
//! JSON (Chrome and Firefox use this framing for native messaging; the pipe reuses it). The
//! pipe accepts local clients only, and each connection must first present the token the
//! app wrote into its data folder, which only the same Windows user can read. Nothing
//! listens on a network port.

mod frame;
mod message;
#[cfg(windows)]
mod pipe;

pub use frame::{MAX_FRAME, read_frame, read_json, write_frame, write_json};
pub use message::{Capture, Link, Reply, Request};
#[cfg(windows)]
pub use pipe::{connect, serve};

use std::path::{Path, PathBuf};

/// The app's identifier (`tauri.conf.json`), which names its data folder.
pub const APP_ID: &str = "io.github.monurpalan.idma";

/// Name of the native messaging host, as registered with the browsers.
pub const HOST_NAME: &str = "io.github.monurpalan.idma";

/// File in the app's data folder holding the current token.
pub const TOKEN_FILE: &str = "bridge.token";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("bad message: {0}")]
    Json(#[from] serde_json::Error),
    #[error("message of {0} bytes is over the limit")]
    TooLarge(usize),
    #[error("the app refused the connection (wrong token)")]
    Refused,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The app's data folder: `IDMA_HOME` when set (a separate profile for trying things out),
/// otherwise `%APPDATA%\<app id>`, where Tauri keeps it.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("IDMA_HOME") {
        return Some(PathBuf::from(home));
    }
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(APP_ID))
}

/// The pipe for this Windows user and data folder: `\\.\pipe\idm-alternative-<user>-<hash>`.
/// Different users, and different `IDMA_HOME` profiles, get different pipes.
pub fn pipe_name(data_dir: &Path) -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    // FNV-1a: stable across runs and builds, unlike the standard hasher.
    let hash = data_dir
        .to_string_lossy()
        .to_lowercase()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
    let user: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    format!(r"\\.\pipe\idm-alternative-{user}-{hash:016x}")
}

/// Writes a fresh random token into `data_dir` and returns it. Called by the app on every
/// start, so a token never outlives the app instance that made it.
pub fn new_token(data_dir: &Path) -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(data_dir.join(TOKEN_FILE), &token)?;
    Ok(token)
}

/// The token the running app wrote, for connecting to it.
pub fn read_token(data_dir: &Path) -> Result<String> {
    Ok(std::fs::read_to_string(data_dir.join(TOKEN_FILE))?
        .trim()
        .to_owned())
}

/// Compares without stopping at the first difference, so timing reveals nothing.
pub(crate) fn same_token(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_names_differ_per_profile() {
        let a = pipe_name(Path::new(
            r"C:\Users\x\AppData\Roaming\io.github.monurpalan.idma",
        ));
        let b = pipe_name(Path::new(r"C:\tmp\idma-ui"));
        assert_ne!(a, b);
        assert!(a.starts_with(r"\\.\pipe\idm-alternative-"));
        // Case-insensitive like Windows paths.
        assert_eq!(b, pipe_name(Path::new(r"C:\TMP\idma-ui")));
    }

    #[test]
    fn tokens() {
        let dir = std::env::temp_dir().join(format!("idma-bridge-token-{}", std::process::id()));
        let t1 = new_token(&dir).expect("token");
        assert_eq!(t1.len(), 64);
        assert_eq!(read_token(&dir).expect("read"), t1);
        let t2 = new_token(&dir).expect("token");
        assert_ne!(t1, t2);
        assert!(same_token(&t2, &read_token(&dir).expect("read")));
        assert!(!same_token(&t1, &t2));
        assert!(!same_token("ab", "abc"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
