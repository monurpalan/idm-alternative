//! The app's settings: what the settings dialog edits, saved in the manager's database.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use shardload_engine::{Browser, Conflict, Options};
use shardload_manager::{Category, Config};

/// Key the settings are saved under.
pub const KEY: &str = "app";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where new downloads go.
    pub dir: PathBuf,
    /// Sort new downloads into `<dir>\Video`, `<dir>\Music`… like IDM.
    pub per_category: bool,
    /// Downloads running at once.
    pub max_active: usize,
    /// Connections per download.
    pub connections: usize,
    /// Replace a finished file with the same name instead of saving as `name (1).ext`.
    pub overwrite: bool,
    /// For all downloads together, bytes per second.
    pub speed_limit: Option<u64>,
    /// Browser to look like: chrome, edge, firefox, safari or none.
    pub browser: String,
    /// `http://`, `https://` or `socks5://host:port`; empty for none.
    pub proxy: String,
    /// Closing the window keeps the app running in the tray.
    pub close_to_tray: bool,
    pub start_with_windows: bool,
    /// A notification when a download finishes.
    pub notify: bool,
    /// Registered with the browsers, so the extension can hand downloads over.
    pub browser_integration: bool,
    /// Update yt-dlp and deno when a new release is out (checked once a day). Sites like
    /// YouTube change often and an old yt-dlp stops working.
    pub update_tools: bool,
    /// Look for a new version of the app once a day and offer it.
    pub check_updates: bool,
    pub schedule: crate::schedule::Schedule,
    /// Offer copied download links, like IDM. Off unless the user turns it on.
    pub watch_clipboard: bool,
    /// Scan each finished download for viruses.
    pub virus_scan: bool,
    /// The scanner's command line, `{file}` for the path; empty for Microsoft Defender.
    pub scanner: String,
    /// A small window per download while it runs, like IDM's.
    pub progress_windows: bool,
    /// "system" (Windows' display language), "en" or "tr".
    pub language: String,
    /// Open magnet links and `.torrent` files with the app. Off unless the user turns it
    /// on: another torrent client may own them.
    pub torrent_links: bool,
    /// Mark finished downloads as coming from the Internet, as browsers do, so Windows
    /// SmartScreen and Office's Protected View check them.
    pub mark_downloads: bool,
    /// For a download without a checksum, look for one the publisher gives (GitHub's
    /// digest of a release asset, `SHA256SUMS` and the like next to the file) and check the
    /// finished file against it.
    pub find_checksums: bool,
    /// Without a checksum, check a finished file by its own structure (ZIP, PDF, PNG, video)
    /// and download damaged parts again.
    pub check_structure: bool,
    /// What happens to finished downloads (see [`shardload_manager::after`]): the only place
    /// a command or a webhook can come from.
    pub after_rules: Vec<shardload_manager::after::Rule>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dir: PathBuf::new(),
            per_category: false,
            max_active: 3,
            connections: 16,
            overwrite: false,
            speed_limit: None,
            browser: "chrome".into(),
            proxy: String::new(),
            close_to_tray: true,
            start_with_windows: false,
            notify: true,
            browser_integration: true,
            update_tools: true,
            check_updates: true,
            schedule: crate::schedule::Schedule::default(),
            watch_clipboard: false,
            virus_scan: false,
            scanner: String::new(),
            progress_windows: false,
            language: "system".into(),
            torrent_links: false,
            mark_downloads: true,
            find_checksums: true,
            check_structure: true,
            after_rules: Vec::new(),
        }
    }
}

impl Settings {
    /// Rejects values the dialog should not have let through.
    pub fn validate(&self) -> Result<(), String> {
        if self.dir.as_os_str().is_empty() {
            return Err("choose a download folder".into());
        }
        if !(1..=50).contains(&self.max_active) {
            return Err("downloads at once must be between 1 and 50".into());
        }
        if !(1..=64).contains(&self.connections) {
            return Err("connections per download must be between 1 and 64".into());
        }
        self.browser()?;
        self.schedule.validate()?;
        for rule in &self.after_rules {
            rule.check()?;
        }
        if !["system", "en", "tr"].contains(&self.language.as_str()) {
            return Err(format!("unknown language {}", self.language));
        }
        let proxy = self.proxy.trim();
        if !proxy.is_empty()
            && !["http://", "https://", "socks5://", "socks5h://"]
                .iter()
                .any(|p| proxy.to_ascii_lowercase().starts_with(p))
        {
            return Err("the proxy must start with http://, https:// or socks5://".into());
        }
        Ok(())
    }

    fn browser(&self) -> Result<Option<Browser>, String> {
        match self.browser.trim().to_ascii_lowercase().as_str() {
            "" | "none" => Ok(None),
            b => b.parse().map(Some),
        }
    }

    /// The manager configuration these settings ask for.
    pub fn config(&self) -> Config {
        let proxy = self.proxy.trim();
        Config {
            dir: self.dir.clone(),
            category_dirs: if self.per_category {
                Category::ALL
                    .iter()
                    .map(|c| (*c, self.dir.join(c.folder())))
                    .collect()
            } else {
                Default::default()
            },
            max_active: self.max_active,
            connections: self.connections,
            conflict: if self.overwrite {
                Conflict::Overwrite
            } else {
                Conflict::Rename
            },
            engine: Options {
                browser: self.browser().unwrap_or(Some(Browser::Chrome)),
                proxy: (!proxy.is_empty()).then(|| proxy.to_owned()),
                mark_of_the_web: self.mark_downloads,
                find_checksum: self.find_checksums,
                check_structure: self.check_structure,
                ..Options::default()
            },
            rules: self.after_rules.clone(),
            ..Config::default()
        }
    }

    /// The form saved in the database: a proxy address with a password is encrypted (DPAPI on
    /// Windows; elsewhere the data folder belongs to the user alone and it stays as it is).
    pub fn to_stored(&self) -> Self {
        let mut stored = self.clone();
        if proxy_password(&self.proxy) {
            stored.proxy = shardload_manager::seal_setting(self.proxy.trim());
        }
        stored
    }

    /// Settings read back from the database. A proxy that cannot be decrypted (the data was
    /// copied from another user or computer) is dropped.
    pub fn restored(mut self) -> Self {
        self.proxy = shardload_manager::open_setting(&self.proxy).unwrap_or_else(|| {
            tracing::warn!("the saved proxy could not be decrypted; it is left empty");
            String::new()
        });
        self
    }

    /// The settings as written to an exported file: without the proxy's password, which would
    /// sit there in plain text. The user name is kept.
    pub fn to_export(&self) -> Self {
        let mut out = self.clone();
        if proxy_password(&self.proxy)
            && let Ok(mut url) = url::Url::parse(self.proxy.trim())
            && url.set_password(None).is_ok()
        {
            out.proxy = url.to_string();
            // `Url` adds a path the user did not type.
            if !self.proxy.trim().ends_with('/') && out.proxy.ends_with('/') {
                out.proxy.pop();
            }
        }
        out
    }

    /// Whether the proxy address carries a password, which [`to_export`](Self::to_export)
    /// leaves out.
    pub fn proxy_has_password(&self) -> bool {
        proxy_password(&self.proxy)
    }
}

/// Whether a proxy address has `user:password@` in it.
fn proxy_password(proxy: &str) -> bool {
    url::Url::parse(proxy.trim()).is_ok_and(|u| u.password().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation() {
        let ok = Settings {
            dir: "C:\\x".into(),
            ..Settings::default()
        };
        assert_eq!(ok.validate(), Ok(()));
        for bad in [
            Settings {
                dir: PathBuf::new(),
                ..ok.clone()
            },
            Settings {
                max_active: 0,
                ..ok.clone()
            },
            Settings {
                connections: 65,
                ..ok.clone()
            },
            Settings {
                browser: "netscape".into(),
                ..ok.clone()
            },
            Settings {
                proxy: "ftp://x".into(),
                ..ok.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn config_follows_settings() {
        let s = Settings {
            dir: "D:\\dl".into(),
            per_category: true,
            overwrite: true,
            browser: "none".into(),
            proxy: " socks5://127.0.0.1:1080 ".into(),
            ..Settings::default()
        };
        let c = s.config();
        assert_eq!(
            c.category_dirs.get(&Category::Video),
            Some(&PathBuf::from("D:\\dl").join("Video"))
        );
        assert_eq!(c.conflict, Conflict::Overwrite);
        assert_eq!(c.engine.browser, None);
        assert_eq!(c.engine.proxy.as_deref(), Some("socks5://127.0.0.1:1080"));
        assert!(c.engine.find_checksum);
        let off = Settings {
            find_checksums: false,
            ..s.clone()
        };
        assert!(!off.config().engine.find_checksum);
        // Older saved settings missing newer fields still load.
        let old: Settings =
            serde_json::from_str(r#"{"dir":"C:\\x","max_active":5}"#).expect("json");
        assert_eq!(old.max_active, 5);
        assert!(old.notify);
        assert!(old.mark_downloads && old.config().engine.mark_of_the_web);
        let off = Settings {
            mark_downloads: false,
            ..Settings::default()
        };
        assert!(!off.config().engine.mark_of_the_web);
        assert!(old.find_checksums);
    }

    #[test]
    fn proxy_password_is_not_kept_in_plain_text() {
        let s = Settings {
            proxy: "http://me:s3cret@127.0.0.1:8080".into(),
            ..Settings::default()
        };
        let stored = s.to_stored();
        #[cfg(windows)]
        assert!(!stored.proxy.contains("s3cret"), "{}", stored.proxy);
        let json = serde_json::to_string(&stored).expect("json");
        let back: Settings = serde_json::from_str(&json).expect("json");
        assert_eq!(back.restored(), s);
        // Without a password there is nothing to hide.
        let plain = Settings {
            proxy: "socks5://127.0.0.1:1080".into(),
            ..Settings::default()
        };
        assert_eq!(plain.to_stored(), plain);
        assert_eq!(plain.clone().restored(), plain);
        // Settings saved before encryption read as they were.
        let old = Settings {
            proxy: "http://me:old@proxy:3128".into(),
            ..Settings::default()
        };
        assert_eq!(old.clone().restored(), old);
        // One that cannot be decrypted is dropped.
        let foreign = Settings {
            proxy: "dpapi:AAAA".into(),
            ..Settings::default()
        };
        assert_eq!(foreign.restored().proxy, "");
    }

    #[test]
    fn export_leaves_the_proxy_password_out() {
        let s = Settings {
            proxy: "http://me:s3cret@127.0.0.1:8080".into(),
            ..Settings::default()
        };
        assert!(s.proxy_has_password());
        assert_eq!(s.to_export().proxy, "http://me@127.0.0.1:8080");
        let plain = Settings {
            proxy: "socks5://127.0.0.1:1080".into(),
            ..Settings::default()
        };
        assert!(!plain.proxy_has_password());
        assert_eq!(plain.to_export(), plain);
    }
}
