//! The bridge between the browser and the app.
//!
//! ```text
//! extension ──native messaging (stdin/stdout)──▶ shardload-host ──named pipe──▶ app
//! ```
//!
//! Both hops carry the same frames: a 4-byte little-endian length and that many bytes of
//! JSON (Chrome and Firefox use this framing for native messaging; the pipe reuses it). The
//! pipe accepts local clients only, and each connection must first present the token the
//! app wrote into its data folder, which only the same Windows user can read. Nothing
//! listens on a network port.

mod client;
mod frame;
mod message;
mod pipe;
#[cfg(windows)]
mod register;
#[cfg(unix)]
#[path = "register_unix.rs"]
mod register;
#[cfg(windows)]
mod torrent_links;
#[cfg(unix)]
#[path = "torrent_links_unix.rs"]
mod torrent_links;

pub use client::{Client, NOT_RUNNING};
pub use frame::{MAX_FRAME, read_frame, read_json, write_frame, write_json};
pub use message::{
    AddDownload, Capture, ExtensionHello, Link, MIN_EXTENSION_PROTOCOL, MediaCapture, MediaItem,
    PROTOCOL, Reply, Request,
};
pub use pipe::{Conn, connect, serve};
pub use register::{installed_browsers, manifest_dir, register, registered_manifest, unregister};
pub use torrent_links::{
    CLASSES, register_torrent_links, torrent_links_registered, unregister_torrent_links,
};

use std::path::{Path, PathBuf};

/// The app's identifier (`tauri.conf.json`), which names its data folder.
pub const APP_ID: &str = "io.github.monurpalan.shardload";

/// Name of the native messaging host, as registered with the browsers.
pub const HOST_NAME: &str = "io.github.monurpalan.shardload";

/// The extension's ID in Chromium browsers (fixed by the public key in its manifest), and in
/// Firefox; the host's registration allows only these to start it.
pub const CHROMIUM_EXTENSION_ID: &str = "calgjlgiljgeegibddigkflnpegdhhle";
pub const FIREFOX_EXTENSION_ID: &str = "shardload@monurpalan.github.io";

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

/// The app's data folder: `SHARDLOAD_HOME` when set (a separate profile for trying things
/// out), otherwise where Tauri keeps it: `%APPDATA%\<app id>` on Windows,
/// `$XDG_DATA_HOME/<app id>` (`~/.local/share/<app id>`) elsewhere.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("SHARDLOAD_HOME") {
        return Some(PathBuf::from(home));
    }
    let dir = default_data_dir()?;
    migrate_legacy_data(&dir);
    Some(dir)
}

fn default_data_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(APP_ID));
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join(APP_ID))
}

/// The identifier the app had before it was called Shardload. Its data folder sits next to
/// the current one, under this name.
const LEGACY_APP_ID: &str = "io.github.monurpalan.idma";

/// Moves what the app kept under its old identifier into the current data folder, once per
/// process.
pub fn migrate_legacy_data(data: &Path) {
    static DONE: std::sync::Once = std::sync::Once::new();
    DONE.call_once(|| move_legacy(data));
}

/// Moves each entry of the old folder into `data`, unless `data` already has one with that
/// name; a file another program holds open stays behind. The old folder is removed once empty.
fn move_legacy(data: &Path) {
    let is_default = data.file_name() == Some(std::ffi::OsStr::new(APP_ID));
    let legacy = data.with_file_name(LEGACY_APP_ID);
    if !is_default || !legacy.is_dir() || std::fs::create_dir_all(data).is_err() {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(&legacy) {
        for entry in entries.flatten() {
            let target = data.join(entry.file_name());
            if !target.exists() {
                let _ = std::fs::rename(entry.path(), target);
            }
        }
    }
    let _ = std::fs::remove_dir(&legacy);
}

#[cfg(test)]
mod legacy_tests {
    use super::*;

    #[test]
    fn moves_the_old_folder_and_keeps_what_the_new_one_has() {
        let base = std::env::temp_dir().join(format!("shardload-legacy-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let legacy = base.join(LEGACY_APP_ID);
        let data = base.join(APP_ID);
        std::fs::create_dir_all(legacy.join("logs")).expect("old folder");
        std::fs::create_dir_all(&data).expect("new folder");
        std::fs::write(legacy.join("downloads.db"), b"list").expect("list");
        std::fs::write(legacy.join("logs").join("a.log"), b"log").expect("log");
        std::fs::write(legacy.join("bridge.token"), b"old").expect("old token");
        std::fs::write(data.join("bridge.token"), b"new").expect("new token");

        move_legacy(&data);

        assert_eq!(
            std::fs::read(data.join("downloads.db")).expect("moved"),
            b"list"
        );
        assert!(data.join("logs").join("a.log").is_file());
        assert_eq!(
            std::fs::read(data.join("bridge.token")).expect("kept"),
            b"new"
        );
        assert!(
            legacy.join("bridge.token").is_file(),
            "a clash stays in the old folder"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}

/// The channel for this user and data folder. On Windows a named pipe,
/// `\\.\pipe\shardload-<user>-<hash>`; elsewhere a socket in the data folder, which only
/// this user can open. Different users, and different `SHARDLOAD_HOME` profiles, get different
/// ones.
pub fn pipe_name(data_dir: &Path) -> String {
    if !cfg!(windows) {
        return data_dir.join("bridge.sock").to_string_lossy().into_owned();
    }
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
    format!(r"\\.\pipe\shardload-{user}-{hash:016x}")
}

/// Writes a fresh random token into `data_dir` and returns it. Called by the app on every
/// start, so a token never outlives the app instance that made it.
pub fn new_token(data_dir: &Path) -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(data_dir)?;
    let path = data_dir.join(TOKEN_FILE);
    std::fs::write(&path, &token)?;
    // Windows keeps the data folder to its user; elsewhere the file itself has to be.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
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
    #[cfg(unix)]
    fn the_socket_is_in_the_data_folder() {
        let a = pipe_name(Path::new(
            "/home/x/.local/share/io.github.monurpalan.shardload",
        ));
        assert_eq!(
            a,
            "/home/x/.local/share/io.github.monurpalan.shardload/bridge.sock"
        );
        assert_ne!(a, pipe_name(Path::new("/tmp/shardload-ui")));
    }

    #[test]
    #[cfg(windows)]
    fn pipe_names_differ_per_profile() {
        let a = pipe_name(Path::new(
            r"C:\Users\x\AppData\Roaming\io.github.monurpalan.shardload",
        ));
        let b = pipe_name(Path::new(r"C:\tmp\shardload-ui"));
        assert_ne!(a, b);
        assert!(a.starts_with(r"\\.\pipe\shardload-"));
        // Case-insensitive like Windows paths.
        assert_eq!(b, pipe_name(Path::new(r"C:\TMP\shardload-ui")));
    }

    #[test]
    fn tokens() {
        let dir =
            std::env::temp_dir().join(format!("shardload-bridge-token-{}", std::process::id()));
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
