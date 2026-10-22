//! The "Mark of the Web": the `Zone.Identifier` stream browsers attach to downloaded files
//! on Windows. SmartScreen and Office's Protected View look for it, so a download the app
//! took over from the browser keeps the protection it would have had. On Linux the same
//! addresses go into the `user.xdg.origin.url` and `user.xdg.referrer.url` attributes, as
//! Chrome and Firefox write them.
//!
//! The mark says the file came from the Internet zone (`ZoneId=3`). Files from hosts that
//! are clearly not on the Internet get none: `localhost` and other loopback addresses,
//! private and link-local IP addresses, single-label host names (`http://nas/`) and
//! `.local` names. Only `http`, `https` and `ftp` URLs are marked.

use std::net::IpAddr;
use std::path::Path;

use url::{Host, Url};

/// The `Zone.Identifier` contents for a file downloaded from `url`, with the page that
/// linked to it; `None` when the file should not be marked. User names and passwords are
/// left out of both URLs, as browsers do.
pub fn zone_identifier(url: &str, referrer: Option<&str>) -> Option<String> {
    let host_url = internet_url(url)?;
    let mut out = String::from("[ZoneTransfer]\r\nZoneId=3\r\n");
    if let Some(r) = referrer.and_then(web_url) {
        out.push_str(&format!("ReferrerUrl={r}\r\n"));
    }
    out.push_str(&format!("HostUrl={host_url}\r\n"));
    Some(out)
}

/// Writes the mark on `path`, downloaded from `url` with the request `headers` (the
/// `Referer` among them says which page it came from). Does nothing off Windows, for
/// hosts that are not on the Internet, or when the file system has no alternate data
/// streams (FAT32, exFAT, some network shares): a missing mark never fails a download.
pub fn mark_of_the_web(path: &Path, url: &str, headers: &[(String, String)]) {
    let referrer = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("referer"))
        .map(|(_, v)| v.as_str());
    let Some(content) = zone_identifier(url, referrer) else {
        return;
    };
    if let Err(e) = write(path, &content, url, referrer) {
        tracing::debug!(path = %path.display(), "no Zone.Identifier written: {e}");
    }
}

#[cfg(windows)]
fn write(path: &Path, content: &str, _url: &str, _referrer: Option<&str>) -> std::io::Result<()> {
    std::fs::write(stream_path(path), content)
}

#[cfg(unix)]
fn write(path: &Path, _content: &str, url: &str, referrer: Option<&str>) -> std::io::Result<()> {
    if let Some(u) = internet_url(url) {
        xattr::set(path, ORIGIN_ATTR, u.as_bytes())?;
    }
    if let Some(r) = referrer.and_then(web_url) {
        xattr::set(path, REFERRER_ATTR, r.as_bytes())?;
    }
    Ok(())
}

#[cfg(not(any(windows, unix)))]
fn write(_path: &Path, _content: &str, _url: &str, _referrer: Option<&str>) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
const ORIGIN_ATTR: &str = "user.xdg.origin.url";
#[cfg(unix)]
const REFERRER_ATTR: &str = "user.xdg.referrer.url";

/// Where a file says it was downloaded from: what a browser (or this app) wrote with it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct FileMark {
    /// The file's own address.
    pub url: Option<String>,
    /// The page it was downloaded from.
    pub referrer: Option<String>,
}

/// The mark on `path`: its `Zone.Identifier` on Windows, its `user.xdg.*` attributes on
/// Linux. `None` when it has none with an address.
pub fn read_mark(path: &Path) -> Option<FileMark> {
    let mark = read(path)?;
    (mark.url.is_some() || mark.referrer.is_some()).then_some(mark)
}

#[cfg(windows)]
fn read(path: &Path) -> Option<FileMark> {
    let text = std::fs::read(stream_path(path)).ok()?;
    Some(parse_zone_identifier(&String::from_utf8_lossy(&text)))
}

#[cfg(unix)]
fn read(path: &Path) -> Option<FileMark> {
    let get = |name| {
        xattr::get(path, name)
            .ok()
            .flatten()
            .map(|v| String::from_utf8_lossy(&v).trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    Some(FileMark {
        url: get(ORIGIN_ATTR),
        referrer: get(REFERRER_ATTR),
    })
}

#[cfg(not(any(windows, unix)))]
fn read(_path: &Path) -> Option<FileMark> {
    None
}

/// `HostUrl` and `ReferrerUrl` of a `Zone.Identifier`.
pub(crate) fn parse_zone_identifier(text: &str) -> FileMark {
    let value = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.trim().split_once('=')?;
            (k.trim().eq_ignore_ascii_case(key) && !v.trim().is_empty())
                .then(|| v.trim().to_owned())
        })
    };
    FileMark {
        url: value("HostUrl"),
        referrer: value("ReferrerUrl"),
    }
}

/// `<path>:Zone.Identifier`, the alternate data stream holding the mark.
#[cfg(windows)]
pub(crate) fn stream_path(path: &Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(":Zone.Identifier");
    s.into()
}

/// `url` without credentials, if it is a web or FTP address on the Internet.
fn internet_url(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    if !matches!(url.scheme(), "http" | "https" | "ftp") || !on_internet(url.host()?) {
        return None;
    }
    clean(url)
}

/// `url` without credentials, if it is a web address at all.
fn web_url(url: &str) -> Option<String> {
    let url = Url::parse(url.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return None;
    }
    clean(url)
}

fn clean(mut url: Url) -> Option<String> {
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    Some(url.into())
}

fn on_internet(host: Host<&str>) -> bool {
    match host {
        Host::Domain(d) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d.contains('.')
                && !d.ends_with(".local")
                && !d.ends_with(".localhost")
                && d != "localhost"
        }
        Host::Ipv4(ip) => public_ip(IpAddr::V4(ip)),
        Host::Ipv6(ip) => public_ip(IpAddr::V6(ip)),
    }
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified())
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => public_ip(IpAddr::V4(v4)),
            None => {
                let first = v6.segments()[0];
                !(v6.is_loopback()
                    || v6.is_unspecified()
                    || (first & 0xfe00) == 0xfc00 // unique local
                    || (first & 0xffc0) == 0xfe80) // link-local
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_browsers_write() {
        let mark = parse_zone_identifier(
            "[ZoneTransfer]
ZoneId=3
ReferrerUrl=https://example.com/page
HostUrl=https://cdn.example.com/a.zip
",
        );
        assert_eq!(mark.url.as_deref(), Some("https://cdn.example.com/a.zip"));
        assert_eq!(mark.referrer.as_deref(), Some("https://example.com/page"));
        // Edge writes `HostUrl=about:internet` for some downloads, and old marks only a zone.
        assert_eq!(
            parse_zone_identifier(
                "[ZoneTransfer]
ZoneId=3
"
            ),
            FileMark::default()
        );
    }

    #[test]
    fn a_written_mark_reads_back() {
        let dir = std::env::temp_dir().join(format!("shardload-mark-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let f = dir.join("a.zip");
        std::fs::write(&f, b"x").expect("write");
        assert_eq!(read_mark(&f), None);
        mark_of_the_web(
            &f,
            "https://user:pw@cdn.example.com/a.zip",
            &[("Referer".into(), "https://example.com/page".into())],
        );
        // A temporary folder without user attributes (an old tmpfs) has no mark to read.
        #[cfg(unix)]
        let marked = xattr::get(&f, ORIGIN_ATTR).is_ok_and(|v| v.is_some());
        #[cfg(not(unix))]
        let marked = cfg!(windows);
        if marked {
            let mark = read_mark(&f).expect("mark");
            assert_eq!(mark.url.as_deref(), Some("https://cdn.example.com/a.zip"));
            assert_eq!(mark.referrer.as_deref(), Some("https://example.com/page"));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn contents() {
        assert_eq!(
            zone_identifier(
                "https://user:pw@cdn.example.com/a.exe?x=1",
                Some("https://example.com/page")
            )
            .as_deref(),
            Some(
                "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://example.com/page\r\n\
                 HostUrl=https://cdn.example.com/a.exe?x=1\r\n"
            )
        );
        // No page, or one that is not a web address: no ReferrerUrl.
        for referrer in [None, Some(""), Some("about:blank"), Some("not a url")] {
            assert_eq!(
                zone_identifier("http://example.com/a.zip", referrer).as_deref(),
                Some("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=http://example.com/a.zip\r\n"),
                "{referrer:?}"
            );
        }
        assert!(zone_identifier("ftp://ftp.example.org/f.iso", None).is_some());
        assert!(zone_identifier("http://8.8.8.8/f", None).is_some());
        assert!(zone_identifier("http://[2001:db8::1]/f", None).is_some());
    }

    #[test]
    fn not_from_the_internet() {
        for url in [
            "http://localhost:8080/a.exe",
            "http://LOCALHOST./a.exe",
            "http://app.localhost/a.exe",
            "http://127.0.0.1/a.exe",
            "http://10.1.2.3/a.exe",
            "http://172.20.0.1/a.exe",
            "http://192.168.1.10/a.exe",
            "http://169.254.0.5/a.exe",
            "http://[::1]/a.exe",
            "http://[fd00::1]/a.exe",
            "http://[fe80::1]/a.exe",
            "http://[::ffff:192.168.0.1]/a.exe",
            "http://nas/share/a.exe",
            "http://printer.local/a.exe",
            "file:///C:/a.exe",
            "magnet:?xt=urn:btih:abc",
            "data:text/plain,hi",
            "garbage",
        ] {
            assert_eq!(zone_identifier(url, None), None, "{url}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn written_to_the_stream() {
        let dir = std::env::temp_dir()
            .join("shardload-engine-unit")
            .join(format!("zone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("setup.exe");
        std::fs::write(&file, b"MZ").expect("file");
        let headers = vec![("Referer".to_owned(), "https://example.com/dl".to_owned())];
        mark_of_the_web(&file, "https://example.com/setup.exe", &headers);
        let mark = std::fs::read_to_string(stream_path(&file)).expect("stream");
        assert_eq!(
            mark,
            "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://example.com/dl\r\n\
             HostUrl=https://example.com/setup.exe\r\n"
        );
        // The file itself is untouched.
        assert_eq!(std::fs::read(&file).expect("read"), b"MZ");
        // A local host leaves no mark.
        let local = dir.join("local.exe");
        std::fs::write(&local, b"MZ").expect("file");
        mark_of_the_web(&local, "http://127.0.0.1/local.exe", &headers);
        assert!(!stream_path(&local).exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
