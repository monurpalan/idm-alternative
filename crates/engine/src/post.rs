//! Telling a web address about something: a POST of JSON, as the user's webhooks get.

use std::time::Duration;

use crate::download::{Options, http_config};
use crate::error::{Error, Result};
use crate::remote::Http;

/// POSTs `body` as JSON to `url` with the client settings in `opts` (its proxy), as a plain
/// client rather than a browser. Returns the status; one outside 2xx is an error.
pub async fn post_json(
    url: &str,
    body: &serde_json::Value,
    opts: &Options,
    timeout: Duration,
) -> Result<u16> {
    let plain = Options {
        browser: None,
        ..opts.clone()
    };
    let http = Http::new(&http_config(&[], &plain))?;
    let request = http
        .client
        .post(url)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send();
    let resp = tokio::time::timeout(timeout, request)
        .await
        .map_err(|_| Error::Stalled(timeout))??;
    let status = resp.status().as_u16();
    if resp.status().is_success() {
        Ok(status)
    } else {
        Err(Error::Status(status))
    }
}
