//! What `serve_file` makes of a request before it answers with the file: redirects, the
//! scenario's refusals, the range asked for, the response's headers, the counts kept.

use super::*;

/// `<file>.zsync`: the `.zsync` of the file the same query serves.
pub(super) fn zsync_of(sc: &Scenario, file: &str, query: Option<&str>) -> Response {
    let mut data = vec![0u8; sc.size.0 as usize];
    crate::content::fill(sc.seed, 0, &mut data);
    let name = file.rsplit('/').next().unwrap_or_default();
    let url = match query {
        Some(q) => format!("{name}?{q}"),
        None => name.to_owned(),
    };
    crate::zsync::make(&data, name, &url, 2048, 3, 5).into_response()
}

/// Notes who asked for the URL, and how.
pub(super) fn record_request(
    url: &UrlState,
    version: Version,
    headers: &HeaderMap,
    extensions: &axum::http::Extensions,
) {
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
}

/// Which GET of the URL this is (counting it), and the range it asks for, kept.
pub(super) fn count_get(url: &UrlState, method: &Method, headers: &HeaderMap) -> u64 {
    if *method == Method::GET {
        url.ranges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(
                headers
                    .get(header::RANGE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_owned(),
            );
        url.gets.fetch_add(1, Ordering::Relaxed) + 1
    } else {
        url.gets.load(Ordering::Relaxed)
    }
}

/// [`count_get`], then for the GET `hang_get` names, holding the request without a word
/// until the client gives up.
pub(super) async fn count_get_or_hang(
    sc: &Scenario,
    url: &UrlState,
    method: &Method,
    headers: &HeaderMap,
) -> u64 {
    let nth_get = count_get(url, method, headers);
    if sc.hang_get == Some(nth_get) && *method == Method::GET {
        std::future::pending::<()>().await;
    }
    nth_get
}

/// The answer to a connection over the scenario's limit.
pub(super) fn too_many_connections(sc: &Scenario) -> Response {
    match sc.refuse_status.and_then(|s| StatusCode::from_u16(s).ok()) {
        Some(status) if status != StatusCode::SERVICE_UNAVAILABLE => {
            text(status, "too many connections\n")
        }
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "1")],
            "too many connections\n",
        )
            .into_response(),
    }
}

/// The status and the bytes (`start..end`) to answer with, by the range asked for; `None`
/// for a range outside the file.
pub(super) fn requested(
    sc: &Scenario,
    headers: &HeaderMap,
    ranges: bool,
    etag: Option<&str>,
    last_modified: &str,
) -> Option<(StatusCode, u64, u64)> {
    let size = sc.size.0;
    let mut status = StatusCode::OK;
    let (mut start, mut end) = (0u64, size); // end is exclusive

    if ranges
        && !sc.range_as_200
        && let Some(range) = headers.get(header::RANGE).and_then(|v| v.to_str().ok())
        && if_range_matches(headers, etag, last_modified)
    {
        match parse_range(range, size) {
            RangeResult::Satisfiable(s, e) => {
                status = StatusCode::PARTIAL_CONTENT;
                (start, end) = (s, e);
            }
            RangeResult::Unsatisfiable => {
                return None;
            }
            RangeResult::Ignore => {}
        }
    }
    Some((status, start, end))
}

/// Redirects to the same file with one redirect less.
pub(super) fn redirect(app: &AppState, sc: &Scenario, uri: &axum::http::Uri) -> Response {
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
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

/// The answer when the scenario refuses this request: the wrong browser, a missing header
/// or login, an expired link.
pub(super) fn refused(sc: &Scenario, headers: &HeaderMap, url: &UrlState) -> Option<Response> {
    if let Some(wanted) = &sc.only_ua {
        let ua = headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if !ua.contains(wanted.as_str()) {
            return Some(text(StatusCode::FORBIDDEN, "browser not allowed\n"));
        }
    }

    if let Some((name, wanted)) = sc.only_header.as_deref().and_then(|h| h.split_once(':')) {
        let given = headers
            .get(name.trim())
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if !given.contains(wanted.trim()) {
            return Some(text(StatusCode::FORBIDDEN, "missing header\n"));
        }
    }

    if let Some(expected) = &sc.auth {
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Basic "))
            .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok());
        if given.as_deref() != Some(expected.as_bytes()) {
            return Some(
                (
                    StatusCode::UNAUTHORIZED,
                    [(header::WWW_AUTHENTICATE, "Basic realm=\"shardload test\"")],
                    "login required\n",
                )
                    .into_response(),
            );
        }
    }

    if let Some(ttl) = sc.ttl {
        let first = *url
            .first_seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(Instant::now);
        if first.elapsed() > Duration::from_secs(ttl) {
            return Some(text(StatusCode::FORBIDDEN, "link expired\n"));
        }
    }
    None
}

/// The response's headers, for bytes `start..end` of the file's `size` answered with
/// `status`.
pub(super) fn response_headers(
    sc: &Scenario,
    ranges: bool,
    etag: Option<&str>,
    last_modified: &str,
    (status, start, end): (StatusCode, u64, u64),
    size: u64,
) -> HeaderMap {
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        header::CONTENT_TYPE,
        sc.ctype
            .as_deref()
            .and_then(|c| HeaderValue::from_str(c).ok())
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    if ranges {
        resp_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    }
    if let Some(etag) = &etag
        && let Ok(v) = HeaderValue::from_str(etag)
    {
        resp_headers.insert(header::ETAG, v);
    }
    if let Ok(v) = HeaderValue::from_str(last_modified) {
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
    resp_headers
}
