//! Small text files read whole, such as checksum lists.

use futures_util::StreamExt;
use wreq::StatusCode;
use wreq::header::{self, HeaderMap};

use super::Http;

/// What [`Http::small_text`] got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fetched {
    Text(String),
    /// Missing, too large or not text.
    Absent,
    /// Unreachable, or the server turns clients away: better not to ask it again soon.
    Refused,
}

impl Http {
    /// A small text file such as a checksum list: [`Fetched::Text`] when the server answers
    /// with success and at most `limit` bytes that are not an HTML page (says no early, from
    /// the headers, when it can); [`Fetched::Refused`] when it cannot be reached or turns
    /// clients away (403, 429, 503).
    pub(crate) async fn small_text(&self, url: &str, limit: usize) -> Fetched {
        let Ok(resp) = self.get(url, HeaderMap::new(), 0).await else {
            return Fetched::Refused;
        };
        if matches!(
            resp.status(),
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        ) {
            return Fetched::Refused;
        }
        if !resp.status().is_success()
            || resp.content_length().is_some_and(|n| n > limit as u64)
            || resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|t| t.to_ascii_lowercase().contains("html"))
        {
            return Fetched::Absent;
        }
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        loop {
            let chunk = match self.timed(stream.next()).await {
                Ok(None) => break,
                Ok(Some(Ok(chunk))) => chunk,
                Ok(Some(Err(_))) | Err(_) => return Fetched::Refused,
            };
            body.extend_from_slice(&chunk);
            if body.len() > limit {
                return Fetched::Absent;
            }
        }
        String::from_utf8(body).map_or(Fetched::Absent, Fetched::Text)
    }
}
