//! Messages from the extension (and from local tools such as `shardload-mcp`) to the app, and
//! the app's replies. The host passes the extension's messages on, refusing the ones only
//! local tools may send (see [`Request::from_browser`]).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// First message on every pipe connection.
    Hello {
        token: String,
    },
    /// Is anyone there? Answered with the app's version.
    Ping,
    /// The extension saying which version it is and which browser it runs in, when the
    /// browser starts and now and then after. Answered with the app's version.
    Extension(ExtensionHello),
    /// The browser started a download and handed it over.
    Download(Capture),
    /// "Download all links": the links of a page, for the user to pick from.
    Links {
        referrer: Option<String>,
        links: Vec<Link>,
    },
    /// Video or audio the extension saw a page load (files, or HLS/DASH playlists).
    Media(MediaCapture),
    /// The download list. Answered in [`Reply::data`] as an array of tasks.
    Tasks,
    /// Adds a download without asking (the tool's user asked for it). Answered with the new
    /// task in [`Reply::data`].
    Add(AddDownload),
    /// Starts (or resumes) these downloads.
    Resume {
        ids: Vec<i64>,
    },
    Pause {
        ids: Vec<i64>,
    },
    /// Removes these downloads from the list, and their files when `delete_files`.
    Remove {
        ids: Vec<i64>,
        #[serde(default)]
        delete_files: bool,
    },
    /// The files a site links to (the site grabber), answered in [`Reply::data`] as an
    /// array of `{url, text, page}`.
    Grab {
        url: String,
        #[serde(default)]
        depth: Option<u32>,
        #[serde(default)]
        same_site: Option<bool>,
    },
    /// What a page of a video site offers (read by yt-dlp): title, duration, heights.
    InspectVideo {
        url: String,
    },
    /// A torrent's name and files, for choosing `torrent_files`.
    InspectTorrent {
        url: String,
    },
    /// The files in a ZIP archive on a server, read without downloading it, for choosing
    /// `archive_entries`.
    InspectArchive {
        url: String,
    },
    /// Answers once none of these downloads is queued or downloading any more, or after
    /// `timeout_secs` (60 by default), with `{finished, downloads}`.
    Wait {
        ids: Vec<i64>,
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
    /// A speed limit in bytes per second, `None` for none: of one download, or of all
    /// together when `id` is absent.
    SpeedLimit {
        #[serde(default)]
        id: Option<i64>,
        #[serde(default)]
        bytes_per_second: Option<u64>,
    },
    /// Moves a download in the queue: `top`, `up`, `down` or `bottom`.
    Move {
        id: i64,
        to: String,
    },
    /// Gives a download another file name; its files on disk follow.
    Rename {
        id: i64,
        name: String,
    },
    /// Moves a download, and its files on disk, into another folder.
    Relocate {
        id: i64,
        dir: String,
    },
    /// What downloads learned about each site (connections allowed, ranges, identity, how
    /// long links last), answered in [`Reply::data`] as an array.
    Sites,
    /// Forgets what was learned about a site (`host` or `host:port`).
    ForgetSite {
        site: String,
    },
    /// What to do once nothing is downloading or waiting any more: `nothing`, `exit`,
    /// `sleep`, `hibernate` or `shutdown`.
    WhenDone {
        action: String,
    },
}

impl Request {
    /// Whether the browser extension may send this. The extension only hands downloads to
    /// the app for the user to confirm; listing, adding without asking and removing are for
    /// tools the user runs, which reach the pipe directly.
    pub fn from_browser(&self) -> bool {
        matches!(
            self,
            Self::Ping
                | Self::Extension(_)
                | Self::Download(_)
                | Self::Links { .. }
                | Self::Media(_)
        )
    }
}

/// The version of the messages between the extension and the app. Raised when a change
/// means an older extension no longer works with the app.
pub const PROTOCOL: u32 = 1;

/// The oldest extension protocol the app still works with.
pub const MIN_EXTENSION_PROTOCOL: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtensionHello {
    /// The extension's version, e.g. `0.1.1`.
    pub version: String,
    /// `chrome`, `edge`, `brave`, `opera`, `vivaldi`, `chromium` or `firefox`.
    pub browser: String,
    /// The [`PROTOCOL`] the extension was built with.
    pub protocol: u32,
}

impl ExtensionHello {
    /// Whether the app works with this extension. A different version alone does not
    /// matter; only an older protocol than the app accepts does.
    pub fn compatible(&self) -> bool {
        self.protocol >= MIN_EXTENSION_PROTOCOL
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AddDownload {
    pub url: String,
    /// Folder to save into: the download folder or a category's folder, or one inside them;
    /// the app's default folder when absent. Anything else is refused.
    pub dir: Option<String>,
    /// File name; the server's suggestion when absent.
    pub name: Option<String>,
    /// `false` adds it paused.
    pub start: Option<bool>,
    /// Other addresses of the same file.
    pub mirrors: Vec<String>,
    /// `algorithm:hex` (sha256, sha512, sha1, md5) the finished file must match.
    pub checksum: Option<String>,
    /// A page of a video site, downloaded through yt-dlp at this height at most.
    pub video_height: Option<u32>,
    /// A page of a video site, its audio alone.
    pub audio_only: Option<bool>,
    /// A torrent: indices of the files to download (see `inspect_torrent`); all when absent.
    pub torrent_files: Option<Vec<usize>>,
    /// A ZIP archive: names (or globs) of the files inside it to download, extracted into a
    /// folder named after it (see `inspect_archive`).
    pub archive_entries: Option<Vec<String>>,
    /// Add it even when it was downloaded before or is on the list already; otherwise such
    /// a request is refused with what was found.
    pub allow_duplicate: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaCapture {
    /// The page playing it; sent as the referrer.
    pub page: Option<String>,
    pub title: Option<String>,
    pub items: Vec<MediaItem>,
    /// `Cookie` header value for the media's site.
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaItem {
    pub url: String,
    /// `hls`, `dash`, `video` or `audio`.
    pub kind: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub mime: Option<String>,
}

impl MediaCapture {
    /// Request headers for fetching the media as the page did.
    pub fn request_headers(&self) -> Vec<(String, String)> {
        Capture {
            referrer: self.page.clone(),
            cookies: self.cookies.clone(),
            user_agent: self.user_agent.clone(),
            ..Capture::default()
        }
        .request_headers()
    }
}

/// Everything needed to download what the browser was downloading, as the browser would.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capture {
    pub url: String,
    /// The page the download started from.
    pub referrer: Option<String>,
    /// The name the browser chose, if any.
    pub filename: Option<String>,
    /// Size the browser was told, if any.
    pub size: Option<u64>,
    /// `Cookie` header value for the URL, from the browser's cookie store.
    pub cookies: Option<String>,
    pub user_agent: Option<String>,
    /// Any other request headers worth repeating (e.g. `Authorization`).
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub url: String,
    /// The link's text on the page.
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The app's version, in answer to `ping`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// What a request asked for, e.g. the tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Reply {
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
            version: None,
            data: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
            version: None,
            data: None,
        }
    }
}

impl Capture {
    /// The request headers for a download of this capture: cookies, user agent, referrer
    /// and the rest, without duplicates.
    pub fn request_headers(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let mut push = |k: &str, v: &str| {
            if !v.is_empty() && !out.iter().any(|(o, _)| o.eq_ignore_ascii_case(k)) {
                out.push((k.to_owned(), v.to_owned()));
            }
        };
        if let Some(c) = &self.cookies {
            push("Cookie", c);
        }
        if let Some(r) = &self.referrer {
            push("Referer", r);
        }
        if let Some(ua) = &self.user_agent {
            push("User-Agent", ua);
        }
        for (k, v) in &self.headers {
            push(k, v);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape() {
        let r: Request = serde_json::from_str(
            r#"{"type":"download","url":"https://x/a.zip","cookies":"s=1","headers":[["Authorization","Bearer t"]]}"#,
        )
        .expect("parse");
        let Request::Download(c) = r else {
            panic!("not a download")
        };
        let m: Request = serde_json::from_str(
            r#"{"type":"media","page":"https://x/watch","items":[{"url":"https://cdn/x.m3u8","kind":"hls"}],"cookies":"a=1"}"#,
        )
        .expect("media");
        let Request::Media(m) = m else {
            panic!("not media")
        };
        assert_eq!(m.items[0].kind, "hls");
        assert_eq!(
            m.request_headers(),
            vec![
                ("Cookie".to_owned(), "a=1".to_owned()),
                ("Referer".to_owned(), "https://x/watch".to_owned())
            ]
        );
        assert_eq!(c.url, "https://x/a.zip");
        assert_eq!(
            c.request_headers(),
            vec![
                ("Cookie".to_owned(), "s=1".to_owned()),
                ("Authorization".to_owned(), "Bearer t".to_owned())
            ]
        );
        assert_eq!(
            serde_json::to_string(&Request::Ping).expect("json"),
            r#"{"type":"ping"}"#
        );
        assert_eq!(
            serde_json::to_string(&Reply::ok()).expect("json"),
            r#"{"ok":true}"#
        );
    }

    #[test]
    fn only_capture_messages_come_from_the_browser() {
        let tasks: Request = serde_json::from_str(r#"{"type":"tasks"}"#).expect("tasks");
        assert!(!tasks.from_browser());
        let remove: Request =
            serde_json::from_str(r#"{"type":"remove","ids":[1,2]}"#).expect("remove");
        assert_eq!(
            remove,
            Request::Remove {
                ids: vec![1, 2],
                delete_files: false
            }
        );
        assert!(!remove.from_browser());
        let add: Request =
            serde_json::from_str(r#"{"type":"add","url":"https://x/a.zip"}"#).expect("add");
        assert!(!add.from_browser());
        let grab: Request =
            serde_json::from_str(r#"{"type":"grab","url":"https://x/"}"#).expect("grab");
        assert!(!grab.from_browser());
        for msg in [
            r#"{"type":"inspect_video","url":"https://x/v"}"#,
            r#"{"type":"inspect_torrent","url":"magnet:?xt=urn:btih:x"}"#,
            r#"{"type":"inspect_archive","url":"https://x/a.zip"}"#,
            r#"{"type":"wait","ids":[1]}"#,
            r#"{"type":"speed_limit","bytes_per_second":1048576}"#,
            r#"{"type":"move","id":1,"to":"top"}"#,
            r#"{"type":"rename","id":1,"name":"a.zip"}"#,
            r#"{"type":"relocate","id":1,"dir":"C:\\Users\\x\\Downloads"}"#,
            r#"{"type":"when_done","action":"sleep"}"#,
        ] {
            let r: Request = serde_json::from_str(msg).expect(msg);
            assert!(!r.from_browser(), "{msg}");
        }
        assert!(Request::Ping.from_browser());
    }

    #[test]
    fn the_extension_says_hello() {
        let r: Request = serde_json::from_str(
            r#"{"type":"extension","version":"0.1.1","browser":"edge","protocol":1}"#,
        )
        .expect("hello");
        assert!(r.from_browser());
        let Request::Extension(hello) = r else {
            panic!("not a hello")
        };
        assert_eq!(hello.browser, "edge");
        assert!(hello.compatible());
        // Sent by an extension from before protocols: too old.
        let old: Request =
            serde_json::from_str(r#"{"type":"extension","version":"0.0.9","browser":"chrome"}"#)
                .expect("old");
        let Request::Extension(old) = old else {
            panic!("not a hello")
        };
        assert_eq!(old.protocol, 0);
        assert!(!old.compatible());
    }

    #[test]
    fn explicit_headers_do_not_duplicate() {
        let c = Capture {
            url: "u".into(),
            referrer: Some("https://page".into()),
            user_agent: Some("UA".into()),
            headers: vec![("referer".into(), "other".into())],
            ..Capture::default()
        };
        let h = c.request_headers();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0], ("Referer".to_owned(), "https://page".to_owned()));
    }
}
