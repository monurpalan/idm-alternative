//! Registering the native messaging host with the browsers (Windows, current user only).
//!
//! Each browser looks under its own `HKCU\Software\…\NativeMessagingHosts\<name>` key for the
//! path of a JSON manifest, which names the host executable and the extensions allowed to
//! start it. Only our own key is ever written or removed.

use std::path::{Path, PathBuf};

use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

use crate::{CHROMIUM_EXTENSION_ID, FIREFOX_EXTENSION_ID, HOST_NAME, Result};

/// The Chromium browsers' keys; each keeps its hosts under `NativeMessagingHosts`. Opera
/// reads Chrome's.
const CHROMIUM_KEYS: [&str; 5] = [
    r"Software\Google\Chrome",
    r"Software\Chromium",
    r"Software\Microsoft\Edge",
    r"Software\BraveSoftware\Brave-Browser",
    r"Software\Vivaldi",
];
const FIREFOX_KEY: &str = r"Software\Mozilla";

/// Where the manifests go, inside the app's data folder.
pub fn manifest_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("native-messaging")
}

/// Writes the manifests for `host` (the host executable, or a script starting it) into
/// `data_dir` and points the supported browsers at them. Only browsers that already have a
/// key for the current user (installed or used) are registered, so nothing is left behind
/// for browsers that are not there; the app registers again on every start and so picks up
/// a browser installed later. Safe to repeat.
pub fn register(host: &Path, data_dir: &Path) -> Result<()> {
    let dir = manifest_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let chromium = dir.join("chromium.json");
    let firefox = dir.join("firefox.json");
    let path = host.to_string_lossy();
    write_json(
        &chromium,
        &serde_json::json!({
            "name": HOST_NAME,
            "description": "idm-alternative download manager",
            "path": path,
            "type": "stdio",
            "allowed_origins": [format!("chrome-extension://{CHROMIUM_EXTENSION_ID}/")],
        }),
    )?;
    write_json(
        &firefox,
        &serde_json::json!({
            "name": HOST_NAME,
            "description": "idm-alternative download manager",
            "path": path,
            "type": "stdio",
            "allowed_extensions": [FIREFOX_EXTENSION_ID],
        }),
    )?;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let browsers = CHROMIUM_KEYS
        .iter()
        .map(|k| (*k, &chromium))
        .chain([(FIREFOX_KEY, &firefox)]);
    for (browser, manifest) in browsers {
        if hkcu.open_subkey(browser).is_err() {
            continue;
        }
        let (key, _) = hkcu.create_subkey(host_key(browser))?;
        key.set_value("", &manifest.to_string_lossy().as_ref())?;
    }
    Ok(())
}

/// Removes our keys (and only ours) from every browser, and the manifests.
pub fn unregister(data_dir: &Path) -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for browser in CHROMIUM_KEYS.iter().chain([&FIREFOX_KEY]) {
        match hkcu.delete_subkey(host_key(browser)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    std::fs::remove_dir_all(manifest_dir(data_dir)).ok();
    Ok(())
}

/// The manifest the browsers are pointed at now (Chrome's key), if any.
pub fn registered_manifest() -> Option<PathBuf> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu.open_subkey(host_key(CHROMIUM_KEYS[0])).ok()?;
    key.get_value::<String, _>("").ok().map(PathBuf::from)
}

fn host_key(browser: &str) -> String {
    format!(r"{browser}\NativeMessagingHosts\{HOST_NAME}")
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
