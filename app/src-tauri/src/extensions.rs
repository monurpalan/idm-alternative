//! The browser extensions the app has heard from: per browser, when and with which version.
//! For the message under the list (no extension found, or one too old for the app) and the
//! status line in the settings.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use shardload_bridge::ExtensionHello;

/// Key the record is saved under.
pub const KEY: &str = "extensions";

/// How long browser integration is on, with no extension heard from, before saying so. The
/// extension says hello within minutes of the browser starting.
pub const GRACE_SECS: u64 = 60 * 60;

/// An extension not heard from for this long counts as gone (removed from the browser).
pub const FORGET_SECS: u64 = 30 * 24 * 60 * 60;

/// Where the Install button leads, until the extension is in the browsers' stores.
pub const INSTALL_URL: &str = "https://github.com/monurpalan/idm-alternative#install";

/// The store pages, by browser, once there are any: one line each, e.g.
/// `("chrome", "https://chromewebstore.google.com/detail/…")`. Browsers missing here get
/// [`INSTALL_URL`].
const STORE_URLS: &[(&str, &str)] = &[];

/// Where to get the extension for `browser` (or for any, when `None`).
pub fn install_url(browser: Option<&str>) -> &'static str {
    browser
        .and_then(|b| STORE_URLS.iter().find(|(name, _)| *name == b))
        .map_or(INSTALL_URL, |(_, url)| url)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    /// The last hello from each browser.
    pub browsers: BTreeMap<String, Sighting>,
    /// When an extension from before hellos last handed something over (it says nothing
    /// about itself, but works).
    pub unnamed_at: Option<u64>,
    /// Since when browser integration is on, for the grace before "not found".
    pub waiting_since: Option<u64>,
    /// "Don't show again" on the "not found" message.
    pub dismissed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sighting {
    pub version: String,
    pub protocol: u32,
    /// Seconds since the Unix epoch.
    pub at: u64,
}

/// What the page shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    /// Each browser the user has or an extension spoke from, in a stable order.
    pub browsers: Vec<BrowserStatus>,
    pub warning: Option<Warning>,
    /// Seconds until the status may change by itself (the grace runs out).
    pub check_in: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BrowserStatus {
    pub browser: String,
    /// `connected`, `outdated` or `missing`.
    pub state: &'static str,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Warning {
    NotFound,
    Outdated { browser: String },
}

/// The order browsers are listed in.
const ORDER: [&str; 7] = [
    "chrome", "edge", "firefox", "brave", "opera", "vivaldi", "chromium",
];

impl Record {
    /// An extension said hello.
    pub fn heard(&mut self, hello: &ExtensionHello, now: u64) {
        let browser = hello.browser.trim().to_ascii_lowercase();
        if browser.is_empty() {
            return;
        }
        self.browsers.insert(
            browser,
            Sighting {
                version: hello.version.clone(),
                protocol: hello.protocol,
                at: now,
            },
        );
    }

    /// Browser integration was turned on or off (or found on at start).
    pub fn integration(&mut self, on: bool, now: u64) {
        if !on {
            self.waiting_since = None;
        } else if self.waiting_since.is_none() {
            self.waiting_since = Some(now);
        }
    }

    pub fn status(&self, integration: bool, installed: &[&str], now: u64) -> Status {
        let recent = |at: u64| now.saturating_sub(at) < FORGET_SECS;
        let mut names: Vec<String> = installed.iter().map(|b| (*b).to_owned()).collect();
        for (name, seen) in &self.browsers {
            if recent(seen.at) && !names.contains(name) {
                names.push(name.clone());
            }
        }
        let rank = |b: &str| ORDER.iter().position(|o| *o == b).unwrap_or(ORDER.len());
        names.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
        let browsers: Vec<BrowserStatus> = names
            .into_iter()
            .map(
                |name| match self.browsers.get(&name).filter(|s| recent(s.at)) {
                    Some(seen) => BrowserStatus {
                        state: if compatible(seen) {
                            "connected"
                        } else {
                            "outdated"
                        },
                        version: Some(seen.version.clone()).filter(|v| !v.is_empty()),
                        browser: name,
                    },
                    None => BrowserStatus {
                        browser: name,
                        state: "missing",
                        version: None,
                    },
                },
            )
            .collect();
        let any_seen =
            browsers.iter().any(|b| b.state != "missing") || self.unnamed_at.is_some_and(recent);
        let mut check_in = None;
        let warning = if !integration {
            None
        } else if let Some(old) = browsers.iter().find(|b| b.state == "outdated") {
            Some(Warning::Outdated {
                browser: old.browser.clone(),
            })
        } else if any_seen || self.dismissed {
            None
        } else {
            let waited = now.saturating_sub(self.waiting_since.unwrap_or(now));
            if waited >= GRACE_SECS {
                Some(Warning::NotFound)
            } else {
                check_in = Some(GRACE_SECS - waited);
                None
            }
        };
        Status {
            browsers,
            warning,
            check_in,
        }
    }
}

fn compatible(seen: &Sighting) -> bool {
    ExtensionHello {
        protocol: seen.protocol,
        ..ExtensionHello::default()
    }
    .compatible()
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_800_000_000;

    fn hello(browser: &str, version: &str, protocol: u32) -> ExtensionHello {
        ExtensionHello {
            version: version.into(),
            browser: browser.into(),
            protocol,
        }
    }

    #[test]
    fn not_found_only_after_the_grace() {
        let mut r = Record::default();
        r.integration(true, T0);
        let s = r.status(true, &["chrome", "firefox"], T0 + 60);
        assert_eq!(s.warning, None);
        assert_eq!(s.check_in, Some(GRACE_SECS - 60));
        assert!(s.browsers.iter().all(|b| b.state == "missing"));
        let s = r.status(true, &["chrome"], T0 + GRACE_SECS);
        assert_eq!(s.warning, Some(Warning::NotFound));
        // Off: nothing to say.
        assert_eq!(r.status(false, &["chrome"], T0 + GRACE_SECS).warning, None);
        // Turned off and on again: a new grace.
        r.integration(false, T0 + GRACE_SECS);
        r.integration(true, T0 + GRACE_SECS + 10);
        assert_eq!(r.status(true, &[], T0 + GRACE_SECS + 20).warning, None);
    }

    #[test]
    fn dont_show_again() {
        let mut r = Record::default();
        r.integration(true, T0);
        r.dismissed = true;
        assert_eq!(
            r.status(true, &["chrome"], T0 + GRACE_SECS * 2).warning,
            None
        );
    }

    #[test]
    fn a_hello_connects_its_browser() {
        let mut r = Record::default();
        r.integration(true, T0);
        r.heard(&hello("Edge", "0.1.1", 1), T0 + 5);
        let s = r.status(true, &["chrome", "edge", "firefox"], T0 + GRACE_SECS);
        assert_eq!(s.warning, None);
        let line: Vec<_> = s
            .browsers
            .iter()
            .map(|b| (b.browser.as_str(), b.state, b.version.as_deref()))
            .collect();
        assert_eq!(
            line,
            [
                ("chrome", "missing", None),
                ("edge", "connected", Some("0.1.1")),
                ("firefox", "missing", None)
            ]
        );
        // A browser the app does not look for (Opera) still shows once it spoke.
        r.heard(&hello("opera", "0.1.1", 1), T0 + 6);
        let s = r.status(true, &["chrome"], T0 + 7);
        let names: Vec<_> = s.browsers.iter().map(|b| b.browser.as_str()).collect();
        assert_eq!(names, ["chrome", "edge", "opera"]);
    }

    #[test]
    fn only_an_older_protocol_is_out_of_date() {
        let mut r = Record::default();
        r.integration(true, T0);
        // Another version with the same protocol works.
        r.heard(&hello("chrome", "0.0.1", shardload_bridge::PROTOCOL), T0);
        assert_eq!(r.status(true, &["chrome"], T0).warning, None);
        r.heard(&hello("firefox", "0.0.1", 0), T0);
        let s = r.status(true, &["chrome"], T0);
        assert_eq!(
            s.warning,
            Some(Warning::Outdated {
                browser: "firefox".into()
            })
        );
        assert_eq!(s.browsers[1].state, "outdated");
        // Even with "don't show again" for the other message.
        r.dismissed = true;
        assert!(r.status(true, &[], T0).warning.is_some());
    }

    #[test]
    fn an_extension_gone_for_a_month_is_forgotten() {
        let mut r = Record::default();
        r.integration(true, T0);
        r.heard(&hello("chrome", "0.1.1", 1), T0);
        let later = T0 + FORGET_SECS;
        let s = r.status(true, &[], later);
        assert!(s.browsers.is_empty());
        assert_eq!(s.warning, Some(Warning::NotFound));
    }

    #[test]
    fn an_old_extension_that_hands_downloads_over_counts() {
        let mut r = Record::default();
        r.integration(true, T0);
        r.unnamed_at = Some(T0 + 1);
        assert_eq!(r.status(true, &["chrome"], T0 + GRACE_SECS).warning, None);
    }

    #[test]
    fn install_links() {
        assert_eq!(install_url(None), INSTALL_URL);
        assert_eq!(install_url(Some("chrome")), INSTALL_URL);
    }
}
