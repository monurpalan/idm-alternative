//! Site memory: what downloads found out about a server (how many connections it allows,
//! whether it sends parts of files, which browser it serves, how long its links work), so
//! the next download from it starts that way instead of finding out again.

use std::time::Duration;

use shardload_engine::{Browser, Learned, Options};

use crate::db::Db;
use crate::{Error, Result};

/// A site's memory is forgotten this long after a download last used or changed it.
pub const FORGET_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// What is remembered about one site.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SiteMemory {
    /// Host, with the port when it is not the scheme's default (see [`crate::site_of`]).
    pub site: String,
    /// It refused connections beyond this many. The next download tries one more, so a
    /// server that allows more again is noticed.
    pub max_connections: Option<usize>,
    /// It does not send parts of files: one connection, from the start.
    pub no_ranges: bool,
    /// The identity it served after refusing the one set in the settings: `chrome`, `edge`,
    /// `firefox`, `safari`, or `none` for a plain client.
    pub browser: Option<String>,
    /// How long a link from it worked before it expired, the shortest seen.
    pub link_lifetime: Option<u64>,
    /// Unix milliseconds.
    pub seen_at: i64,
}

impl SiteMemory {
    pub(crate) fn is_empty(&self) -> bool {
        self.max_connections.is_none()
            && !self.no_ranges
            && self.browser.is_none()
            && self.link_lifetime.is_none()
    }

    /// Download options for this site: at most one connection more than it allowed, one
    /// without ranges, and the identity it accepts.
    pub fn apply(&self, opts: &mut Options) {
        if let Some(max) = self.max_connections {
            opts.connections = opts.connections.min(max + 1);
        }
        if self.no_ranges {
            opts.connections = 1;
        }
        if let Some(b) = &self.browser {
            opts.browser = parse_browser(b);
        }
    }

    /// Takes in what a download learned. `tried`: the connections it was allowed;
    /// `wanted`: the connections downloads get without a limit; `finished`: whether it got
    /// through to the end (so "no ranges" not seen again means the server sends them now).
    pub fn learn(&mut self, learned: &Learned, tried: usize, wanted: usize, finished: bool) {
        match learned.max_connections {
            Some(n) => self.max_connections = Some(n),
            // It ran with all it was allowed and none was refused: the server gave way.
            None if learned.peak_connections >= tried && tried > 1 => {
                self.max_connections = Some(tried).filter(|t| *t < wanted);
            }
            None => {}
        }
        if learned.no_ranges {
            self.no_ranges = true;
        } else if finished {
            self.no_ranges = false;
        }
    }

    /// A link of the site stopped working this long after it was added.
    pub fn expired_after(&mut self, secs: u64) {
        self.link_lifetime = Some(self.link_lifetime.map_or(secs, |l| l.min(secs)));
    }
}

/// `chrome`… or `none` → the identity to present.
pub fn parse_browser(name: &str) -> Option<Browser> {
    name.parse().ok()
}

pub fn browser_name(b: Option<Browser>) -> &'static str {
    match b {
        Some(Browser::Chrome) => "chrome",
        Some(Browser::Edge) => "edge",
        Some(Browser::Firefox) => "firefox",
        Some(Browser::Safari) => "safari",
        None => "none",
    }
}

/// Identities to try, in order, when a site refuses the one set: the common browsers first.
pub const IDENTITIES: [Option<Browser>; 5] = [
    Some(Browser::Chrome),
    Some(Browser::Firefox),
    Some(Browser::Edge),
    Some(Browser::Safari),
    None,
];

/// The memory of `site`, unless it is older than [`FORGET_AFTER`] (then it is deleted).
pub(crate) async fn recall(db: &Db, site: &str, now: i64) -> Result<Option<SiteMemory>> {
    let Some(m) = db.site(site).await? else {
        return Ok(None);
    };
    if now - m.seen_at > FORGET_AFTER.as_millis() as i64 {
        db.forget_site(site).await?;
        return Ok(None);
    }
    Ok(Some(m))
}

/// Saves `m` as seen `now`, or forgets the site when nothing is left to remember.
pub(crate) async fn keep(db: &Db, mut m: SiteMemory, now: i64) -> Result<(), Error> {
    if m.is_empty() {
        return db.forget_site(&m.site).await;
    }
    m.seen_at = now;
    db.save_site(&m).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn learned(max: Option<usize>, peak: usize, no_ranges: bool) -> Learned {
        Learned {
            max_connections: max,
            peak_connections: peak,
            no_ranges,
        }
    }

    #[test]
    fn limits_are_learned_and_given_up_when_the_server_relents() {
        let mut m = SiteMemory::default();
        m.learn(&learned(Some(4), 5, false), 16, 16, false);
        assert_eq!(m.max_connections, Some(4));
        let mut o = Options {
            connections: 16,
            ..Options::default()
        };
        m.apply(&mut o);
        assert_eq!(o.connections, 5, "one more than allowed");
        // Refused again at 5: stays 4.
        m.learn(&learned(Some(4), 5, false), 5, 16, true);
        assert_eq!(m.max_connections, Some(4));
        // A small file that never opened them all says nothing.
        m.learn(&learned(None, 2, false), 5, 16, true);
        assert_eq!(m.max_connections, Some(4));
        // All 5 ran: the server allows 5 now.
        m.learn(&learned(None, 5, false), 5, 16, true);
        assert_eq!(m.max_connections, Some(5));
        // Up to what downloads want anyway: no limit left.
        m.learn(&learned(None, 16, false), 16, 16, true);
        assert_eq!(m.max_connections, None);
        assert!(m.is_empty());
    }

    #[test]
    fn ranges_and_identity_and_lifetime() {
        let mut m = SiteMemory::default();
        m.learn(&learned(None, 1, true), 16, 16, true);
        assert!(m.no_ranges);
        m.learn(&learned(None, 1, false), 1, 16, false);
        assert!(m.no_ranges, "an unfinished download proves nothing");
        m.browser = Some("firefox".into());
        let mut o = Options::default();
        m.apply(&mut o);
        assert_eq!((o.connections, o.browser), (1, Some(Browser::Firefox)));
        m.browser = Some("none".into());
        m.apply(&mut o);
        assert_eq!(o.browser, None);
        m.expired_after(600);
        m.expired_after(900);
        assert_eq!(m.link_lifetime, Some(600));
        m.learn(&learned(None, 1, false), 1, 16, true);
        assert!(!m.no_ranges);
        for b in IDENTITIES {
            assert_eq!(parse_browser(browser_name(b)), b);
        }
    }
}
