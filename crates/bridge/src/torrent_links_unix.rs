//! Opening magnet links and `.torrent` files with the app (Linux and other Unix desktops,
//! current user only), through a desktop entry and `xdg-mime`.
//!
//! Offered as a setting, off by default: another torrent client may own these. Turning it
//! off removes our desktop entry, so the desktop falls back to the other handlers.

use std::path::{Path, PathBuf};

use crate::Result;

/// Empty: the user's own applications folder (`~/.local/share/applications`). Tests pass a
/// folder of their own, and then the desktop's defaults are left alone.
pub const CLASSES: &str = "";
const DESKTOP_FILE: &str = "shardload-torrent.desktop";
const TYPES: [&str; 2] = ["x-scheme-handler/magnet", "application/x-bittorrent"];

fn applications(classes: &str) -> Option<PathBuf> {
    if !classes.is_empty() {
        return Some(PathBuf::from(classes));
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("applications"))
}

fn entry(app: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Shardload\nComment=Download torrents and magnet links\nExec=\"{}\" %u\nNoDisplay=true\nMimeType={};\n",
        app.display(),
        TYPES.join(";")
    )
}

/// Makes `app` the handler of `magnet:` links and `.torrent` files.
pub fn register_torrent_links(app: &Path, classes: &str) -> Result<()> {
    let dir = applications(classes).ok_or_else(|| std::io::Error::other("HOME is not set"))?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(DESKTOP_FILE), entry(app))?;
    if classes.is_empty() {
        for mime in TYPES {
            std::process::Command::new("xdg-mime")
                .args(["default", DESKTOP_FILE, mime])
                .status()
                .ok();
        }
    }
    Ok(())
}

/// Takes back what [`register_torrent_links`] set.
pub fn unregister_torrent_links(_app: &Path, classes: &str) -> Result<()> {
    if let Some(dir) = applications(classes) {
        match std::fs::remove_file(dir.join(DESKTOP_FILE)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Whether our desktop entry names `app`.
pub fn torrent_links_registered(app: &Path, classes: &str) -> bool {
    applications(classes)
        .and_then(|d| std::fs::read_to_string(d.join(DESKTOP_FILE)).ok())
        .is_some_and(|text| text == entry(app))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_and_removes_its_entry() {
        let dir = std::env::temp_dir().join(format!("shardload-links-{}", std::process::id()));
        let classes = dir.to_string_lossy().into_owned();
        let app = Path::new("/opt/shardload/shardload");
        register_torrent_links(app, &classes).expect("register");
        assert!(torrent_links_registered(app, &classes));
        let text = std::fs::read_to_string(dir.join(DESKTOP_FILE)).expect("entry");
        assert!(text.contains("x-scheme-handler/magnet;application/x-bittorrent;"));
        unregister_torrent_links(app, &classes).expect("unregister");
        assert!(!torrent_links_registered(app, &classes));
        std::fs::remove_dir_all(dir).ok();
    }
}
