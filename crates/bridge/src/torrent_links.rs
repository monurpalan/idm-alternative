//! Opening magnet links and `.torrent` files with the app (Windows, current user only).
//!
//! Offered as a setting, off by default: another torrent client may own these, and turning it
//! on must not take them over silently. Windows still lets the user's own choice (the
//! "UserChoice" it keeps) win, and asks which app to use when there is more than one. Turning
//! it off removes only what points at us.

use std::path::Path;

use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

use crate::Result;

/// Where per-user file types and URL schemes live.
pub const CLASSES: &str = r"Software\Classes";
const PROG_ID: &str = "shardload.torrent";

/// Makes `app` the handler of `magnet:` links and `.torrent` files, under `classes`
/// ([`CLASSES`]; tests use another key).
pub fn register_torrent_links(app: &Path, classes: &str) -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let command = command(app);
    let icon = format!("\"{}\",0", app.display());

    let (magnet, _) = hkcu.create_subkey(format!(r"{classes}\magnet"))?;
    magnet.set_value("", &"URL:Magnet link")?;
    magnet.set_value("URL Protocol", &"")?;
    magnet
        .create_subkey("DefaultIcon")?
        .0
        .set_value("", &icon)?;
    magnet
        .create_subkey(r"shell\open\command")?
        .0
        .set_value("", &command)?;

    let (prog, _) = hkcu.create_subkey(format!(r"{classes}\{PROG_ID}"))?;
    prog.set_value("", &"Torrent file")?;
    prog.create_subkey("DefaultIcon")?.0.set_value("", &icon)?;
    prog.create_subkey(r"shell\open\command")?
        .0
        .set_value("", &command)?;

    let (ext, _) = hkcu.create_subkey(format!(r"{classes}\.torrent"))?;
    ext.set_value("", &PROG_ID)?;
    ext.set_value("Content Type", &"application/x-bittorrent")?;
    ext.create_subkey("OpenWithProgids")?
        .0
        .set_value(PROG_ID, &"")?;
    Ok(())
}

/// Takes back what [`register_torrent_links`] set, leaving anything another app put there.
pub fn unregister_torrent_links(app: &Path, classes: &str) -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let ours = |key: &str| {
        hkcu.open_subkey(format!(r"{classes}\{key}\shell\open\command"))
            .and_then(|k| k.get_value::<String, _>(""))
            .is_ok_and(|c| c.eq_ignore_ascii_case(&command(app)))
    };
    if ours("magnet") {
        delete_tree(&hkcu, &format!(r"{classes}\magnet"))?;
    }
    delete_tree(&hkcu, &format!(r"{classes}\{PROG_ID}"))?;
    if let Ok(ext) = hkcu.open_subkey_with_flags(
        format!(r"{classes}\.torrent"),
        winreg::enums::KEY_READ | winreg::enums::KEY_WRITE,
    ) {
        if ext.get_value::<String, _>("").is_ok_and(|v| v == PROG_ID) {
            ext.delete_value("").ok();
        }
        if let Ok(with) = ext.open_subkey_with_flags("OpenWithProgids", winreg::enums::KEY_WRITE) {
            with.delete_value(PROG_ID).ok();
        }
    }
    Ok(())
}

/// Whether `magnet:` links open with `app` now (as far as our own key says).
pub fn torrent_links_registered(app: &Path, classes: &str) -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(format!(r"{classes}\magnet\shell\open\command"))
        .and_then(|k| k.get_value::<String, _>(""))
        .is_ok_and(|c| c.eq_ignore_ascii_case(&command(app)))
}

fn command(app: &Path) -> String {
    format!("\"{}\" \"%1\"", app.display())
}

fn delete_tree(hkcu: &RegKey, key: &str) -> Result<()> {
    match hkcu.delete_subkey_all(key) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_and_removes_only_its_own() {
        let classes = format!(r"Software\shardload-tests-{}\Classes", std::process::id());
        let app = Path::new(r"C:\Program Files\Shardload\shardload.exe");
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        register_torrent_links(app, &classes).expect("register");
        assert!(torrent_links_registered(app, &classes));
        let ext = hkcu
            .open_subkey(format!(r"{classes}\.torrent"))
            .expect(".torrent");
        assert_eq!(ext.get_value::<String, _>("").expect("default"), PROG_ID);

        // Another client takes .torrent files over; turning ours off leaves its choice.
        drop(ext);
        let ext = hkcu
            .create_subkey(format!(r"{classes}\.torrent"))
            .expect("open")
            .0;
        ext.set_value("", &"other.torrent").expect("set");
        unregister_torrent_links(app, &classes).expect("unregister");
        assert!(!torrent_links_registered(app, &classes));
        assert_eq!(
            ext.get_value::<String, _>("").expect("default"),
            "other.torrent"
        );
        assert!(hkcu.open_subkey(format!(r"{classes}\{PROG_ID}")).is_err());

        // A magnet handler of another app is not ours to remove.
        let (other, _) = hkcu
            .create_subkey(format!(r"{classes}\magnet\shell\open\command"))
            .expect("other");
        other
            .set_value("", &"\"C:\\other.exe\" \"%1\"")
            .expect("set");
        unregister_torrent_links(app, &classes).expect("unregister");
        assert!(
            hkcu.open_subkey(format!(r"{classes}\magnet")).is_ok(),
            "removed another app's magnet handler"
        );

        hkcu.delete_subkey_all(format!(r"Software\shardload-tests-{}", std::process::id()))
            .expect("clean up");
    }
}
