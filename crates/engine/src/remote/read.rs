//! Waiting on a server within limits set by the read timeout. Its answer (connecting,
//! redirects, the server's own time before the status and headers) may take
//! [`ANSWER_TIMEOUTS`] read timeouts: some servers build the file before they answer. Each
//! piece of a body read whole must come within one. A server that keeps sending, however
//! slowly, is never cut off; one that goes quiet fails as [`Error::Stalled`].

use std::future::Future;

use futures_util::StreamExt;
use wreq::Response;

use super::Http;
use crate::error::{Error, Result};

/// How many read timeouts the wait for an answer may take.
const ANSWER_TIMEOUTS: u32 = 3;

impl Http {
    /// `send`, the wait for an answer, failing as stalled after [`ANSWER_TIMEOUTS`] read
    /// timeouts (when there is a read timeout).
    pub(crate) async fn answer<T>(&self, send: impl Future<Output = T>) -> Result<T> {
        match self.read_timeout {
            Some(limit) => {
                // A read timeout too long to triple (meaning no limit) stays that long.
                let limit = limit.saturating_mul(ANSWER_TIMEOUTS);
                tokio::time::timeout(limit, send)
                    .await
                    .map_err(|_| Error::Stalled(limit))
            }
            None => Ok(send.await),
        }
    }

    /// `f`, failing as stalled when it takes longer than the read timeout (when there is one).
    pub(crate) async fn timed<T>(&self, f: impl Future<Output = T>) -> Result<T> {
        match self.read_timeout {
            Some(limit) => tokio::time::timeout(limit, f)
                .await
                .map_err(|_| Error::Stalled(limit)),
            None => Ok(f.await),
        }
    }

    /// The whole body of `resp`, each piece of it awaited within the read timeout.
    pub(crate) async fn read_body(&self, resp: Response) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = self.timed(stream.next()).await? {
            body.extend_from_slice(&chunk?);
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::remote::HttpConfig;

    #[tokio::test]
    async fn a_read_timeout_too_long_to_triple_still_waits_for_the_answer() {
        let http = Http::new(&HttpConfig {
            read_timeout: Some(Duration::MAX),
            ..HttpConfig::default()
        })
        .expect("client");
        assert_eq!(http.answer(async { 7 }).await.expect("answered"), 7);
    }
}
