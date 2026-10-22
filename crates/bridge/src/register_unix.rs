//! Registering the native messaging host with the browsers (Linux and other Unix systems,
//! current user only).
//!
//! Each browser reads host manifests from a folder of its own: the Chromium family from
//! `~/.config/<browser>/NativeMessagingHosts/`, Firefox from
//! `~/.mozilla/native-messaging-hosts/`. Only our own manifest file is ever written or
//! removed, and only for browsers whose folder exists (installed or used). Browsers packaged
//! as Snap or Flatpak keep their own folders and are not reached.

use std::path::{Path, PathBuf};

use crate::{CHROMIUM_EXTENSION_ID, FIREFOX_EXTENSION_ID, HOST_NAME, Result};

/// The Chromium browsers' folders under `~/.config`.
const CHROMIUM_DIRS: [&str; 5] = [
    "google-chrome",
    "chromium",
    "microsoft-edge",
    "BraveSoftware/Brave-Browser",
    "vivaldi",
];
/// The browsers of [`CHROMIUM_DIRS`], as the extension names them.
const CHROMIUM_NAMES: [&str; 5] = ["chrome", "chromium", "edge", "brave", "vivaldi"];

/// Kept for the same API as on Windows, where the manifests live in the data folder; here
/// they go straight into the browsers' folders.
pub fn manifest_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("native-messaging")
}

fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home().map(|h| h.join(".config")))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `(browser folder, manifest path, is Firefox)` for every supported browser.
fn targets() -> Vec<(PathBuf, PathBuf, bool)> {
    named_targets().into_iter().map(|(_, t)| t).collect()
}

/// [`targets`] with each browser's name, as the extension names it.
fn named_targets() -> Vec<(&'static str, (PathBuf, PathBuf, bool))> {
    let file = format!("{HOST_NAME}.json");
    let mut out = Vec::new();
    if let Some(config) = config_home() {
        for (dir, name) in CHROMIUM_DIRS.into_iter().zip(CHROMIUM_NAMES) {
            let browser = config.join(dir);
            let manifest = browser.join("NativeMessagingHosts").join(&file);
            out.push((name, (browser, manifest, false)));
        }
    }
    if let Some(home) = home() {
        let browser = home.join(".mozilla");
        let manifest = browser.join("native-messaging-hosts").join(&file);
        out.push(("firefox", (browser, manifest, true)));
    }
    out
}

/// The supported browsers this user has (by their folders): `chrome`, `chromium`, `edge`,
/// `brave`, `vivaldi`, `firefox`.
pub fn installed_browsers() -> Vec<&'static str> {
    named_targets()
        .into_iter()
        .filter(|(_, (browser, _, _))| browser.is_dir())
        .map(|(name, _)| name)
        .collect()
}

/// Writes our manifest, naming `host` (the host executable, or a script starting it), into
/// each browser's folder. Safe to repeat; the app registers again on every start and so
/// picks up a browser installed later.
pub fn register(host: &Path, _data_dir: &Path) -> Result<()> {
    let path = host.to_string_lossy();
    for (browser, manifest, firefox) in targets() {
        if !browser.is_dir() {
            continue;
        }
        let allowed = if firefox {
            serde_json::json!({ "allowed_extensions": [FIREFOX_EXTENSION_ID] })
        } else {
            serde_json::json!({
                "allowed_origins": [format!("chrome-extension://{CHROMIUM_EXTENSION_ID}/")]
            })
        };
        let mut value = serde_json::json!({
            "name": HOST_NAME,
            "description": "Shardload download manager",
            "path": path,
            "type": "stdio",
        });
        if let (Some(v), Some(a)) = (value.as_object_mut(), allowed.as_object()) {
            v.extend(a.clone());
        }
        if let Some(dir) = manifest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&manifest, serde_json::to_vec_pretty(&value)?)?;
    }
    Ok(())
}

/// Removes our manifest (and only ours) from every browser.
pub fn unregister(data_dir: &Path) -> Result<()> {
    for (_, manifest, _) in targets() {
        match std::fs::remove_file(&manifest) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    std::fs::remove_dir_all(manifest_dir(data_dir)).ok();
    Ok(())
}

/// The manifest Chrome is pointed at now, if any.
pub fn registered_manifest() -> Option<PathBuf> {
    targets()
        .into_iter()
        .map(|(_, manifest, _)| manifest)
        .find(|m| m.is_file())
}
