//! The app's settings: what the settings dialog edits, saved in the manager's database.

use std::path::PathBuf;

use idma_engine::{Browser, Conflict, Options};
use idma_manager::{Category, Config};
use serde::{Deserialize, Serialize};

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
                ..Options::default()
            },
            ..Config::default()
        }
    }
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
        // Older saved settings missing newer fields still load.
        let old: Settings =
            serde_json::from_str(r#"{"dir":"C:\\x","max_active":5}"#).expect("json");
        assert_eq!(old.max_active, 5);
        assert!(old.notify);
    }
}
