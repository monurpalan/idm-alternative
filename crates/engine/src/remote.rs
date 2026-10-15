//! Talking to the server: the first request, range requests, and checking that every
//! response is really the slice of the same file that was asked for.

use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Response, StatusCode};
use serde::{Deserialize, Serialize};

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
}

#[derive(Clone)]
pub struct Http {
    pub client: reqwest::Client,
    pub headers: HeaderMap,
}

impl Http {
    pub fn new(user_agent: &str, extra: &[(String, String)], connections: usize) -> Result<Self> {
        let mut headers = HeaderMap::new();
        // Compressed bodies break byte offsets.
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        for (k, v) in extra {
            let name = HeaderName::try_from(k.as_str())
                .map_err(|e| Error::State(format!("bad header name `{k}`: {e}")))?;
            let value = HeaderValue::try_from(v.as_str())
                .map_err(|e| Error::State(format!("bad header value for `{k}`: {e}")))?;
            headers.insert(name, value);
        }
        let client = reqwest::Client::builder()
            .user_agent(user_agent)
            // One TCP connection per segment: HTTP/2 would multiplex every segment over a
            // single connection and lose the point of segmenting.
            .http1_only()
            .connect_timeout(std::time::Duration::from_secs(15))
            .pool_max_idle_per_host(connections)
            .build()?;
        Ok(Self { client, headers })
    }

    async fn get(&self, url: &str, extra: HeaderMap) -> Result<Response> {
        let mut req = self.client.get(url).headers(self.headers.clone());
        req = req.headers(extra);
        Ok(req.send().await?)
    }

    /// First request: asks for `bytes=start-` so one round trip tells us the size, whether
    /// ranges work, and the validators, and the body can be used right away.
    pub async fn probe(
        &self,
        url: &str,
        start: u64,
        if_range: Option<&str>,
    ) -> Result<(Remote, Response)> {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, range_value(start, None));
        if let Some(v) = if_range.and_then(|v| HeaderValue::from_str(v).ok()) {
            h.insert(header::IF_RANGE, v);
        }
        let resp = self.get(url, h).await?;
        let validators = Validators::from_headers(resp.headers());
        let html = is_html(&resp);
        let final_url = resp.url().to_string();
        let remote = match resp.status() {
            StatusCode::PARTIAL_CONTENT => {
                let (first, _, total) = content_range(&resp)?;
                if first != start {
                    return Err(Error::BadRange(format!("asked for {start}, got {first}")));
                }
                Remote {
                    url: final_url,
                    size: total,
                    ranges: true,
                    validators,
                    html,
                }
            }
            StatusCode::OK => Remote {
                url: final_url,
                size: resp.content_length(),
                ranges: false,
                validators,
                html,
            },
            StatusCode::RANGE_NOT_SATISFIABLE if start == 0 => {
                // Only an empty file makes `bytes=0-` unsatisfiable.
                Remote {
                    url: final_url,
                    size: Some(0),
                    ranges: false,
                    validators,
                    html,
                }
            }
            s => return Err(status_error(s)),
        };
        Ok((remote, resp))
    }

    /// Requests `[start, end)` of the file and checks the response is exactly that slice of
    /// the same file. `end == None` asks for everything from `start`.
    pub async fn range(&self, remote: &Remote, start: u64, end: Option<u64>) -> Result<Response> {
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
        let resp = self.get(&remote.url, h).await?;
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
            s => Err(status_error(s)),
        }
    }
}

fn status_error(s: StatusCode) -> Error {
    match s {
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
