//! The HTTP router: `/file/{name}?<scenario>` plus `/_stats` and `/_reset`.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, OriginalUri, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Version, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use bytes::Bytes;
use serde::Serialize;

use crate::content;
use crate::scenario::{EtagMode, Scenario};

/// Largest chunk written to the socket at once.
const CHUNK: u64 = 64 << 10;

/// Fixed Last-Modified base: 2026-01-01T00:00:00Z.
const LAST_MODIFIED_BASE: u64 = 1_767_225_600;

/// Per-URL counters. The key is the full request URI, so every distinct test URL
/// gets its own state (connection numbering, TTL clock, validator generation).
#[derive(Default)]
pub(crate) struct UrlState {
    first_seen: Mutex<Option<Instant>>,
    pub(crate) gets: AtomicU64,
    active: AtomicU32,
    peak_active: AtomicU32,
    pub(crate) bytes_sent: AtomicU64,
    generation: AtomicU64,
    h2_gets: AtomicU64,
    /// Distinct client addresses, i.e. TCP connections (needs connect info).
    peers: Mutex<HashSet<SocketAddr>>,
    user_agent: Mutex<Option<String>>,
}

#[derive(Default)]
pub(crate) struct AppState {
    urls: Mutex<HashMap<String, Arc<UrlState>>>,
    next_conn_id: AtomicU64,
    next_hop: AtomicU64,
}

impl AppState {
    pub(crate) fn url(&self, key: &str) -> Arc<UrlState> {
        let mut urls = self.urls.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(urls.entry(key.to_owned()).or_default())
    }
}

/// Builds the test server router.
pub fn router() -> Router {
    Router::new()
        .route("/file/{name}", get(serve_file))
        .route("/hls/master.m3u8", get(crate::hls::master))
        .route("/hls/key", get(crate::hls::key))
        .route("/hls/{playlist}", get(crate::hls::media_playlist))
        .route("/hls/{track}/{segment}", get(crate::hls::segment))
        .route("/dash/manifest.mpd", get(crate::dash::manifest))
        .route("/dash/{rep}/{file}", get(crate::dash::segment))
        .route("/_stats", get(stats))
        .route("/_reset", post(reset))
        .with_state(Arc::new(AppState::default()))
}

#[derive(Serialize)]
struct UrlStats {
    url: String,
    gets: u64,
    active: u32,
    peak_active: u32,
    bytes_sent: u64,
    /// Requests that arrived over HTTP/2.
    h2_gets: u64,
    /// TCP connections the requests came on; 0 when the server has no connect info.
    connections: usize,
    user_agent: Option<String>,
}

async fn stats(State(app): State<Arc<AppState>>) -> axum::Json<Vec<UrlStats>> {
    let urls = app.urls.lock().unwrap_or_else(PoisonError::into_inner);
    let mut out: Vec<UrlStats> = urls
        .iter()
        .map(|(url, s)| UrlStats {
            url: url.clone(),
            gets: s.gets.load(Ordering::Relaxed),
            active: s.active.load(Ordering::Relaxed),
            peak_active: s.peak_active.load(Ordering::Relaxed),
            bytes_sent: s.bytes_sent.load(Ordering::Relaxed),
            h2_gets: s.h2_gets.load(Ordering::Relaxed),
            connections: s.peers.lock().unwrap_or_else(PoisonError::into_inner).len(),
            user_agent: s
                .user_agent
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        })
        .collect();
    out.sort_by(|a, b| a.url.cmp(&b.url));
    axum::Json(out)
}

async fn reset(State(app): State<Arc<AppState>>) -> StatusCode {
    app.urls
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    StatusCode::NO_CONTENT
}

fn text(status: StatusCode, body: &'static str) -> Response {
    (status, body).into_response()
}

async fn serve_file(
    State(app): State<Arc<AppState>>,
    Query(sc): Query<Scenario>,
    OriginalUri(uri): OriginalUri,
    parts: Parts,
) -> Response {
    let Parts {
        method,
        version,
        headers,
        extensions,
        ..
    } = parts;
    if sc.latency_ms > 0 {
        tokio::time::sleep(Duration::from_millis(sc.latency_ms)).await;
    }

    if sc.redirect > 0 {
        let query = uri.query().unwrap_or_default();
        let mut next: Vec<String> = query
            .split('&')
            .filter(|kv| !kv.starts_with("hop="))
            .map(|kv| {
                if kv.starts_with("redirect=") {
                    format!("redirect={}", sc.redirect - 1)
                } else {
                    kv.to_owned()
                }
            })
            .collect();
        if sc.redirect_unique {
            next.push(format!(
                "hop={}",
                app.next_hop.fetch_add(1, Ordering::Relaxed)
            ));
        }
        // The path as received, still percent-encoded: a decoded non-ASCII name is not a
        // valid Location.
        let location = format!("{}?{}", uri.path(), next.join("&"));
        return (StatusCode::FOUND, [(header::LOCATION, location)]).into_response();
    }

    let url = app.url(&uri.to_string());
    if version == Version::HTTP_2 {
        url.h2_gets.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(ConnectInfo(peer)) = extensions.get::<ConnectInfo<SocketAddr>>() {
        url.peers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(*peer);
    }
    *url.user_agent
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    if let Some(expected) = &sc.auth {
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Basic "))
            .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok());
        if given.as_deref() != Some(expected.as_bytes()) {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Basic realm=\"idma test\"")],
                "login required\n",
            )
                .into_response();
        }
    }

    if let Some(ttl) = sc.ttl {
        let first = *url
            .first_seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(Instant::now);
        if first.elapsed() > Duration::from_secs(ttl) {
            return text(StatusCode::FORBIDDEN, "link expired\n");
        }
    }

    let nth_get = if method == Method::GET {
        url.gets.fetch_add(1, Ordering::Relaxed) + 1
    } else {
        url.gets.load(Ordering::Relaxed)
    };

    if sc.html_error_after.is_some_and(|n| nth_get > n) {
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<!doctype html><html><body><h1>Session expired</h1><p>Please log in again.</p></body></html>",
        )
            .into_response();
    }

    let active = url.active.fetch_add(1, Ordering::AcqRel) + 1;
    if sc.max_conns.is_some_and(|max| active > max) {
        url.active.fetch_sub(1, Ordering::AcqRel);
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "1")],
            "too many connections\n",
        )
            .into_response();
    }
    url.peak_active.fetch_max(active, Ordering::Relaxed);
    // From here on the connection guard owns the `active` slot.
    let mut guard = ConnGuard {
        url: Arc::clone(&url),
        id: app.next_conn_id.fetch_add(1, Ordering::Relaxed) + 1,
        nth_get,
        range: String::from("full"),
        sent: 0,
        started: Instant::now(),
        expected: 0,
    };

    let generation = match sc.etag {
        EtagMode::Rotate => url.generation.fetch_add(1, Ordering::Relaxed),
        EtagMode::Static | EtagMode::None => 0,
    };
    let size = sc.size.0;
    let etag =
        (sc.etag != EtagMode::None).then(|| format!("\"{:x}-{size:x}-{generation}\"", sc.seed));
    let last_modified = httpdate::fmt_http_date(
        SystemTime::UNIX_EPOCH + Duration::from_secs(LAST_MODIFIED_BASE + generation),
    );

    let ranges = sc.ranges && !sc.no_length;
    let mut status = StatusCode::OK;
    let (mut start, mut end) = (0u64, size); // end is exclusive

    if ranges
        && !sc.range_as_200
        && let Some(range) = headers.get(header::RANGE).and_then(|v| v.to_str().ok())
        && if_range_matches(&headers, etag.as_deref(), &last_modified)
    {
        match parse_range(range, size) {
            RangeResult::Satisfiable(s, e) => {
                status = StatusCode::PARTIAL_CONTENT;
                (start, end) = (s, e);
            }
            RangeResult::Unsatisfiable => {
                return (
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [(header::CONTENT_RANGE, format!("bytes */{size}"))],
                )
                    .into_response();
            }
            RangeResult::Ignore => {}
        }
    }

    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    if ranges {
        resp_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    }
    if let Some(etag) = &etag
        && let Ok(v) = HeaderValue::from_str(etag)
    {
        resp_headers.insert(header::ETAG, v);
    }
    if let Ok(v) = HeaderValue::from_str(&last_modified) {
        resp_headers.insert(header::LAST_MODIFIED, v);
    }
    if !sc.no_length
        && let Ok(v) = HeaderValue::from_str(&(end - start).to_string())
    {
        resp_headers.insert(header::CONTENT_LENGTH, v);
    }
    if status == StatusCode::PARTIAL_CONTENT
        && let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{}/{size}", end - 1))
    {
        resp_headers.insert(header::CONTENT_RANGE, v);
    }
    if let Some(cd) = &sc.cd
        && let Ok(v) = HeaderValue::from_str(&content_disposition(cd))
    {
        resp_headers.insert(header::CONTENT_DISPOSITION, v);
    }

    let rate = match sc.slow_conn {
        Some(slow) if slow.nth == nth_get => Some(slow.rate.0),
        _ => sc.rate.map(|r| r.0),
    };
    guard.range = if status == StatusCode::PARTIAL_CONTENT {
        format!("{start}-{}", end - 1)
    } else {
        String::from("full")
    };
    guard.expected = end - start;
    let body = BodyState {
        guard,
        seed: sc.seed,
        pos: start,
        end,
        rate: rate.filter(|r| *r > 0),
        stall_after: sc.stall_after.map(|s| s.0),
        drop_after: sc.drop_after.map(|s| s.0),
    };

    let mut resp = Response::new(Body::from_stream(body.into_stream()));
    *resp.status_mut() = status;
    *resp.headers_mut() = resp_headers;
    resp
}

/// `true` when there is no `If-Range`, or when it still matches the current validator.
fn if_range_matches(headers: &HeaderMap, etag: Option<&str>, last_modified: &str) -> bool {
    let Some(v) = headers.get(header::IF_RANGE).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let v = v.trim();
    if v.starts_with('"') || v.starts_with("W/") {
        // Weak validators never match for range requests (RFC 9110 §13.1.5).
        etag.is_some_and(|e| e == v)
    } else {
        v == last_modified
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RangeResult {
    /// Serve `[start, end)`.
    Satisfiable(u64, u64),
    Unsatisfiable,
    /// Malformed or multi-range: serve the whole file with 200.
    Ignore,
}

fn parse_range(value: &str, size: u64) -> RangeResult {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return RangeResult::Ignore;
    };
    if spec.contains(',') {
        return RangeResult::Ignore;
    }
    let Some((a, b)) = spec.trim().split_once('-') else {
        return RangeResult::Ignore;
    };
    let (a, b) = (a.trim(), b.trim());
    match (a.is_empty(), b.is_empty()) {
        // bytes=-N: last N bytes
        (true, false) => match b.parse::<u64>() {
            Ok(0) => RangeResult::Unsatisfiable,
            Ok(_) if size == 0 => RangeResult::Unsatisfiable,
            Ok(n) => RangeResult::Satisfiable(size.saturating_sub(n), size),
            Err(_) => RangeResult::Ignore,
        },
        // bytes=A- or bytes=A-B
        (false, _) => {
            let Ok(start) = a.parse::<u64>() else {
                return RangeResult::Ignore;
            };
            let last = if b.is_empty() {
                size.saturating_sub(1)
            } else {
                match b.parse::<u64>() {
                    Ok(last) if last >= start => last.min(size.saturating_sub(1)),
                    _ => return RangeResult::Ignore,
                }
            };
            if start >= size {
                RangeResult::Unsatisfiable
            } else {
                RangeResult::Satisfiable(start, last + 1)
            }
        }
        (true, true) => RangeResult::Ignore,
    }
}

fn content_disposition(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii() && c != '"' && !c.is_ascii_control() {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// Tracks one in-flight response; logs it and releases the connection slot when dropped
/// (on completion, on client disconnect, or on an injected failure).
struct ConnGuard {
    url: Arc<UrlState>,
    id: u64,
    nth_get: u64,
    range: String,
    sent: u64,
    started: Instant,
    /// Body length promised to the client. hyper stops polling once `Content-Length`
    /// bytes are out, so completion is judged by bytes sent, not by end-of-stream.
    expected: u64,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.url.active.fetch_sub(1, Ordering::AcqRel);
        let secs = self.started.elapsed().as_secs_f64();
        tracing::info!(
            conn = self.id,
            get = self.nth_get,
            range = %self.range,
            sent = self.sent,
            secs = format!("{secs:.2}"),
            mib_s = format!("{:.2}", self.sent as f64 / secs.max(1e-9) / (1 << 20) as f64),
            complete = self.sent >= self.expected,
            "response finished"
        );
    }
}

struct BodyState {
    guard: ConnGuard,
    seed: u64,
    pos: u64,
    end: u64,
    rate: Option<u64>,
    stall_after: Option<u64>,
    drop_after: Option<u64>,
}

impl BodyState {
    fn into_stream(self) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
        futures_util::stream::unfold(Some(self), |state| async move {
            let mut s = state?; // `None` after an injected drop: end the stream
            if s.pos >= s.end {
                return None;
            }
            let sent = s.guard.sent;
            if s.drop_after.is_some_and(|d| sent >= d) {
                // Let the bytes already written reach the client before the reset;
                // otherwise hyper discards its unflushed buffer along with the connection.
                tokio::time::sleep(Duration::from_millis(100)).await;
                drop(s);
                return Some((Err(std::io::Error::other("injected connection drop")), None));
            }
            if s.stall_after.is_some_and(|d| sent >= d) {
                // Hold the connection open without sending anything until the client gives up.
                std::future::pending::<()>().await;
            }

            let mut len = CHUNK.min(s.end - s.pos);
            for limit in [s.drop_after, s.stall_after].into_iter().flatten() {
                if limit > sent {
                    len = len.min(limit - sent);
                }
            }
            if let Some(rate) = s.rate {
                // Small chunks keep throttled streams smooth (about 20 writes per second).
                len = len.min((rate / 20).max(1024));
            }

            let mut buf = vec![0u8; len as usize];
            content::fill(s.seed, s.pos, &mut buf);
            s.pos += len;
            s.guard.sent += len;
            // Counted live (not on completion) so `/_stats` can be sampled mid-download.
            s.guard.url.bytes_sent.fetch_add(len, Ordering::Relaxed);

            if let Some(rate) = s.rate {
                let due =
                    s.guard.started + Duration::from_secs_f64(s.guard.sent as f64 / rate as f64);
                tokio::time::sleep_until(due.into()).await;
            }
            Some((Ok(Bytes::from(buf)), Some(s)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing() {
        use RangeResult::*;
        assert_eq!(parse_range("bytes=0-99", 1000), Satisfiable(0, 100));
        assert_eq!(parse_range("bytes=900-", 1000), Satisfiable(900, 1000));
        assert_eq!(parse_range("bytes=900-5000", 1000), Satisfiable(900, 1000));
        assert_eq!(parse_range("bytes=-100", 1000), Satisfiable(900, 1000));
        assert_eq!(parse_range("bytes=1000-", 1000), Unsatisfiable);
        assert_eq!(parse_range("bytes=0-1,5-9", 1000), Ignore);
        assert_eq!(parse_range("bytes=9-1", 1000), Ignore);
        assert_eq!(parse_range("items=0-1", 1000), Ignore);
    }

    #[test]
    fn content_disposition_encodes_unicode() {
        assert_eq!(
            content_disposition("çay.zip"),
            "attachment; filename=\"_ay.zip\"; filename*=UTF-8''%C3%A7ay.zip"
        );
    }
}
