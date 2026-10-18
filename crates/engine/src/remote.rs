//! Talking to the server: the first request, range requests, and checking that every
//! response is really the slice of the same file that was asked for.

use serde::{Deserialize, Serialize};
use wreq::header::{self, HeaderMap, HeaderName, HeaderValue};
use wreq::{Response, StatusCode};

use crate::error::{Error, Result};

/// What identifies one version of the remote file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl Validators {
    fn from_headers(h: &HeaderMap) -> Self {
        let get = |name| {
            h.get(name)
                .and_then(|v: &HeaderValue| v.to_str().ok())
                .map(str::to_owned)
        };
        Self {
            etag: get(header::ETAG),
            last_modified: get(header::LAST_MODIFIED),
        }
    }

    /// Value for `If-Range`: a strong ETag if there is one, else Last-Modified.
    /// Weak ETags are not allowed in `If-Range` (RFC 9110 §13.1.5).
    pub fn if_range(&self) -> Option<&str> {
        self.etag
            .as_deref()
            .filter(|e| !e.starts_with("W/"))
            .or(self.last_modified.as_deref())
    }

    /// `false` only when both sides carry the same kind of validator and it differs.
    pub(crate) fn same_file(&self, other: &Validators) -> bool {
        match (&self.etag, &other.etag) {
            (Some(a), Some(b)) => return a == b,
            (Some(_), None) | (None, Some(_)) => return false,
            (None, None) => {}
        }
        match (&self.last_modified, &other.last_modified) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        }
    }
}

/// Facts learned from the first response.
#[derive(Debug, Clone)]
pub struct Remote {
    /// URL after redirects; later requests go here directly.
    pub url: String,
    pub size: Option<u64>,
    pub ranges: bool,
    pub validators: Validators,
    pub html: bool,
    /// File name suggested by the response (see [`crate::name::choose`]).
    pub name: String,
}

impl Remote {
    /// What a response to the first request (for `url`) tells about the file.
    fn from_response(resp: &Response, url: &str, size: Option<u64>, ranges: bool) -> Self {
        let final_url = resp.uri().to_string();
        let name = crate::name::choose(
            resp.headers()
                .get(header::CONTENT_DISPOSITION)
                .map(HeaderValue::as_bytes),
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            &final_url,
            url,
        );
        Self {
            url: final_url,
            size,
            ranges,
            validators: Validators::from_headers(resp.headers()),
            html: is_html(resp),
            name,
        }
    }
}

/// Browsers whose TLS and HTTP/2 fingerprint and default headers the engine can present.
/// Some CDNs and file hosts refuse or throttle clients that do not look like a browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Chrome,
    Edge,
    Firefox,
    Safari,
}

impl Browser {
    /// The newest profile of this browser, on Windows (Safari: macOS).
    fn emulation(self) -> wreq_util::Emulation {
        use wreq_util::{Platform, Profile};
        let (profile, platform) = match self {
            Browser::Chrome => (Profile::Chrome149, Platform::Windows),
            Browser::Edge => (Profile::Edge148, Platform::Windows),
            Browser::Firefox => (Profile::Firefox151, Platform::Windows),
            Browser::Safari => (Profile::Safari26_4, Platform::MacOS),
        };
        wreq_util::Emulation::builder()
            .profile(profile)
            .platform(platform)
            .build()
    }
}

impl std::str::FromStr for Browser {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "chrome" => Ok(Browser::Chrome),
            "edge" => Ok(Browser::Edge),
            "firefox" => Ok(Browser::Firefox),
            "safari" => Ok(Browser::Safari),
            other => Err(format!(
                "unknown browser `{other}` (chrome, edge, firefox, safari)"
            )),
        }
    }
}

/// How the client connects; everything here is fixed for the whole download.
#[derive(Debug, Clone, Default)]
pub struct HttpConfig {
    /// `None` sends the browser's own User-Agent (or the engine's without a browser).
    pub user_agent: Option<String>,
    /// Fingerprint to present; `None` is a plain client.
    pub browser: Option<Browser>,
    /// `http://`, `https://` or `socks5://` proxy for every request.
    pub proxy: Option<String>,
    /// Accept invalid or self-signed certificates.
    pub insecure: bool,
    /// Never negotiate HTTP/2.
    pub http1_only: bool,
    /// Extra headers for every request, e.g. `Cookie`, `Referer`, `Authorization`.
    pub headers: Vec<(String, String)>,
}

#[derive(Clone)]
pub struct Http {
    pub client: wreq::Client,
    pub headers: HeaderMap,
}

impl Http {
    pub fn new(cfg: &HttpConfig) -> Result<Self> {
        let mut headers = HeaderMap::new();
        // Compressed bodies break byte offsets.
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        for (k, v) in &cfg.headers {
            let name = HeaderName::try_from(k.as_str())
                .map_err(|e| Error::State(format!("bad header name `{k}`: {e}")))?;
            let value = HeaderValue::try_from(v.as_str())
                .map_err(|e| Error::State(format!("bad header value for `{k}`: {e}")))?;
            headers.insert(name, value);
        }
        let mut builder = wreq::Client::builder()
            .redirect(wreq::redirect::Policy::limited(10))
            // Cookies set while redirecting (common on file hosts) are sent back.
            .cookie_store(true)
            .connect_timeout(std::time::Duration::from_secs(15))
            .tls_cert_verification(!cfg.insecure)
            .tls_verify_hostname(!cfg.insecure);
        if let Some(browser) = cfg.browser {
            builder = builder.emulation(browser.emulation());
        }
        match (&cfg.user_agent, cfg.browser) {
            (Some(ua), _) => builder = builder.user_agent(ua.as_str()),
            (None, None) => {
                builder = builder.user_agent(format!("idm-alternative/{}", crate::VERSION));
            }
            (None, Some(_)) => {}
        }
        if let Some(proxy) = &cfg.proxy {
            builder = builder.proxy(wreq::Proxy::all(proxy.as_str())?);
        }
        if cfg.http1_only {
            builder = builder.http1_only();
        }
        Ok(Self {
            client: builder.build()?,
            headers,
        })
    }

    /// `slot` picks the connection: each slot has its own pool, so with HTTP/2 segments
    /// still get separate TCP connections instead of being multiplexed over one, and a
    /// slot's connection is reused by whichever segment runs on it next.
    async fn get(&self, url: &str, extra: HeaderMap, slot: u64) -> Result<Response> {
        let mut headers = self.headers.clone();
        headers.extend(extra);
        Ok(self
            .client
            .get(url)
            .headers(headers)
            .group(wreq::Group::new(slot))
            .send()
            .await?)
    }

    /// First request: asks for `bytes=start-` so one round trip tells us the size, whether
    /// ranges work, and the validators, and the body can be used right away.
    pub async fn probe(
        &self,
        url: &str,
        start: u64,
        if_range: Option<&str>,
        slot: u64,
    ) -> Result<(Remote, Response)> {
        self.probe_range(url, start, None, if_range, slot).await
    }

    /// Like [`Http::probe`] but asks only for the first `len` bytes and reads them: for
    /// checking another address of a file without making its server send the rest. A server
    /// without ranges answers with the whole file, which is then not read.
    pub(crate) async fn probe_sample(
        &self,
        url: &str,
        len: u64,
        slot: u64,
    ) -> Result<(Remote, Option<Vec<u8>>)> {
        let (remote, resp) = self
            .probe_range(url, 0, Some(len.max(1)), None, slot)
            .await?;
        if !remote.ranges {
            return Ok((remote, None));
        }
        let body = resp.bytes().await?.to_vec();
        Ok((remote, Some(body)))
    }

    async fn probe_range(
        &self,
        url: &str,
        start: u64,
        end: Option<u64>,
        if_range: Option<&str>,
        slot: u64,
    ) -> Result<(Remote, Response)> {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, range_value(start, end));
        if let Some(v) = if_range.and_then(|v| HeaderValue::from_str(v).ok()) {
            h.insert(header::IF_RANGE, v);
        }
        let resp = self.get(url, h, slot).await?;
        let (size, ranges) = match resp.status() {
            StatusCode::PARTIAL_CONTENT => {
                let (first, _, total) = content_range(&resp)?;
                if first != start {
                    return Err(Error::BadRange(format!("asked for {start}, got {first}")));
                }
                (total, true)
            }
            StatusCode::OK => (resp.content_length(), false),
            // Only an empty file makes `bytes=0-` unsatisfiable.
            StatusCode::RANGE_NOT_SATISFIABLE if start == 0 => (Some(0), false),
            _ => return Err(status_error(&resp)),
        };
        Ok((Remote::from_response(&resp, url, size, ranges), resp))
    }

    /// First request for the end of a file: asks for its last `len` bytes (`bytes=-len`), so
    /// one round trip tells the size, whether ranges work, and brings the bytes. Returns
    /// where the bytes start in the file and the bytes, or `None` when the server ignores
    /// ranges (its response is not read).
    pub(crate) async fn probe_tail(
        &self,
        url: &str,
        len: u64,
    ) -> Result<(Remote, Option<(u64, Vec<u8>)>)> {
        let mut h = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&format!("bytes=-{}", len.max(1))) {
            h.insert(header::RANGE, v);
        }
        let resp = self.get(url, h, 0).await?;
        match resp.status() {
            StatusCode::PARTIAL_CONTENT => {
                let (first, last, total) = content_range(&resp)?;
                let remote = Remote::from_response(&resp, url, total, total.is_some());
                let body = resp.bytes().await?.to_vec();
                if body.len() as u64 != last - first + 1 {
                    return Err(Error::Truncated);
                }
                Ok((remote, Some((first, body))))
            }
            StatusCode::OK => {
                let remote = Remote::from_response(&resp, url, resp.content_length(), false);
                Ok((remote, None))
            }
            // Only an empty file has no last bytes.
            StatusCode::RANGE_NOT_SATISFIABLE => Ok((
                Remote::from_response(&resp, url, Some(0), true),
                Some((0, Vec::new())),
            )),
            _ => Err(status_error(&resp)),
        }
    }

    /// Requests `[start, end)` of the file and checks the response is exactly that slice of
    /// the same file. `end == None` asks for everything from `start`.
    pub async fn range(
        &self,
        remote: &Remote,
        start: u64,
        end: Option<u64>,
        slot: u64,
    ) -> Result<Response> {
        let mut h = HeaderMap::new();
        if remote.ranges {
            h.insert(header::RANGE, range_value(start, end));
            if let Some(v) = remote
                .validators
                .if_range()
                .and_then(|v| HeaderValue::from_str(v).ok())
            {
                h.insert(header::IF_RANGE, v);
            }
        }
        let resp = self.get(&remote.url, h, slot).await?;
        let status = resp.status();
        if (status == StatusCode::OK || status == StatusCode::PARTIAL_CONTENT)
            && is_html(&resp)
            && !remote.html
        {
            return Err(Error::HtmlResponse);
        }
        match status {
            StatusCode::PARTIAL_CONTENT if remote.ranges => {
                let (first, last, total) = content_range(&resp)?;
                if first != start {
                    return Err(Error::BadRange(format!("asked for {start}, got {first}")));
                }
                if let (Some(want), Some(got)) = (remote.size, total)
                    && want != got
                {
                    return Err(Error::FileChanged);
                }
                if let Some(end) = end
                    && last + 1 > end
                {
                    return Err(Error::BadRange(format!(
                        "asked up to {}, got {last}",
                        end - 1
                    )));
                }
                Ok(resp)
            }
            StatusCode::OK if !remote.ranges => Ok(resp),
            StatusCode::OK => {
                if remote
                    .validators
                    .same_file(&Validators::from_headers(resp.headers()))
                {
                    Err(Error::RangeIgnored)
                } else {
                    Err(Error::FileChanged)
                }
            }
            _ => Err(status_error(&resp)),
        }
    }
}

impl Http {
    /// A GET for a whole resource or `[start, end)` of it (`end` exclusive), failing on any
    /// status but 2xx: for stream segments, which are fetched whole and not resumed.
    pub(crate) async fn segment(
        &self,
        url: &str,
        range: Option<(u64, u64)>,
        slot: u64,
    ) -> Result<Response> {
        let mut h = HeaderMap::new();
        if let Some((start, end)) = range {
            h.insert(header::RANGE, range_value(start, Some(end)));
        }
        let resp = self.get(url, h, slot).await?;
        if resp.status().is_success() {
            Ok(resp)
        } else {
            Err(status_error(&resp))
        }
    }

    /// A whole small resource as text (a playlist), and the URL it came from after
    /// redirects. Fails past `limit` bytes.
    pub async fn text(&self, url: &str, limit: usize) -> Result<(String, String)> {
        use futures_util::StreamExt;
        let resp = self.get(url, HeaderMap::new(), 0).await?;
        if !resp.status().is_success() {
            return Err(status_error(&resp));
        }
        let final_url = resp.uri().to_string();
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk?);
            if body.len() > limit {
                return Err(Error::State(format!("{url} is larger than {limit} bytes")));
            }
        }
        Ok((String::from_utf8_lossy(&body).into_owned(), final_url))
    }

    /// A small text file such as a checksum list: `None` unless the server answers with
    /// success and at most `limit` bytes that are not an HTML page. Says no early, from the
    /// headers, when it can.
    pub(crate) async fn small_text(&self, url: &str, limit: usize) -> Option<String> {
        use futures_util::StreamExt;
        let resp = self.get(url, HeaderMap::new(), 0).await.ok()?;
        if !resp.status().is_success()
            || resp.content_length().is_some_and(|n| n > limit as u64)
            || resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|t| t.to_ascii_lowercase().contains("html"))
        {
            return None;
        }
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk.ok()?);
            if body.len() > limit {
                return None;
            }
        }
        String::from_utf8(body).ok()
    }

    /// Downloads `[start, end)` whole; for small checks, not for file data.
    pub async fn fetch(&self, remote: &Remote, start: u64, end: u64, slot: u64) -> Result<Vec<u8>> {
        let resp = self.range(remote, start, Some(end), slot).await?;
        Ok(resp.bytes().await?.to_vec())
    }
}

fn status_error(resp: &Response) -> Error {
    let s = resp.status();
    match s {
        // A login prompt, not a link that stopped working: signed links that expire answer
        // 401 without asking for credentials.
        StatusCode::UNAUTHORIZED
            if let Some(challenge) = resp.headers().get(header::WWW_AUTHENTICATE) =>
        {
            Error::AuthRequired {
                realm: challenge.to_str().ok().and_then(realm),
            }
        }
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::GONE => {
            Error::LinkExpired(s.as_u16())
        }
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => Error::Busy(s.as_u16()),
        s => Error::Status(s.as_u16()),
    }
}

fn range_value(start: u64, end: Option<u64>) -> HeaderValue {
    let v = match end {
        Some(end) => format!("bytes={start}-{}", end - 1),
        None => format!("bytes={start}-"),
    };
    HeaderValue::from_str(&v).unwrap_or_else(|_| HeaderValue::from_static("bytes=0-"))
}

/// The `realm` of a `WWW-Authenticate` challenge, shown to the user.
fn realm(challenge: &str) -> Option<String> {
    let start = challenge.to_ascii_lowercase().find("realm=")? + "realm=".len();
    let rest = &challenge[start..];
    let value = match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next()?,
        None => rest.split([',', ' ']).next()?,
    };
    Some(value.to_owned()).filter(|v| !v.is_empty())
}

/// An `Authorization` header for HTTP Basic credentials.
pub fn basic_auth(user: &str, password: &str) -> (String, String) {
    use base64::Engine;
    let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    ("Authorization".to_owned(), format!("Basic {token}"))
}

fn is_html(resp: &Response) -> bool {
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| {
            ct.trim_start()
                .to_ascii_lowercase()
                .starts_with("text/html")
        })
}

/// Parses `Content-Range: bytes first-last/total` (total may be `*`).
fn content_range(resp: &Response) -> Result<(u64, u64, Option<u64>)> {
    let raw = resp
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Error::BadRange("206 without Content-Range".into()))?;
    parse_content_range(raw)
        .ok_or_else(|| Error::BadRange(format!("unparsable Content-Range `{raw}`")))
}

fn parse_content_range(raw: &str) -> Option<(u64, u64, Option<u64>)> {
    let spec = raw.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = spec.split_once('/')?;
    let (first, last) = range.trim().split_once('-')?;
    let (first, last): (u64, u64) = (first.trim().parse().ok()?, last.trim().parse().ok()?);
    if last < first {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some((first, last, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_parsing() {
        assert_eq!(
            parse_content_range("bytes 0-99/1000"),
            Some((0, 99, Some(1000)))
        );
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, 9, None)));
        assert_eq!(parse_content_range("bytes 9-5/10"), None);
        assert_eq!(parse_content_range("items 0-1/2"), None);
    }

    #[test]
    fn if_range_prefers_strong_etag() {
        let v = Validators {
            etag: Some("\"a\"".into()),
            last_modified: Some("lm".into()),
        };
        assert_eq!(v.if_range(), Some("\"a\""));
        let weak = Validators {
            etag: Some("W/\"a\"".into()),
            last_modified: Some("lm".into()),
        };
        assert_eq!(weak.if_range(), Some("lm"));
    }

    #[test]
    fn same_file_compares_like_with_like() {
        let a = Validators {
            etag: Some("\"1\"".into()),
            last_modified: None,
        };
        let b = Validators {
            etag: Some("\"2\"".into()),
            last_modified: None,
        };
        assert!(a.same_file(&a));
        assert!(!a.same_file(&b));
        assert!(!a.same_file(&Validators::default()));
        assert!(Validators::default().same_file(&Validators::default()));
    }
}
