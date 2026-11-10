//! Waiting on a server within the read timeout: for its answer, and for each piece of a body
//! read whole. A server that keeps sending, however slowly, is never cut off; one that goes
//! quiet fails as [`Error::Stalled`].

use std::future::Future;

use futures_util::StreamExt;
use wreq::Response;

use super::Http;
use crate::error::{Error, Result};

impl Http {
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
