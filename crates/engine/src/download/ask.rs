//! Asking a server about a file before downloading it, and fetching small files whole.

use super::*;

/// What a server says about a URL before anything is downloaded, for deciding where to save
/// it and showing it to the user.
#[derive(Debug, Clone)]
pub struct Probe {
    /// URL after redirects.
    pub url: String,
    pub size: Option<u64>,
    /// Range requests work: the download can use several connections and resume.
    pub resumable: bool,
    /// Safe file name from `Content-Disposition`, the URL or the media type.
    pub name: String,
    /// The server's validators for the file, to tell later whether it changed.
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Asks the server about `url` with the same client settings a download would use, reading
/// only the response headers.
pub async fn probe(url: &str, headers: &[(String, String)], opts: &Options) -> Result<Probe> {
    let http = Http::new(&http_config(headers, opts))?;
    let (remote, resp) = http.probe(url, 0, None, 0).await?;
    drop(resp);
    Ok(Probe {
        url: remote.url,
        size: remote.size,
        resumable: remote.ranges && remote.size.is_some(),
        name: remote.name,
        etag: remote.validators.etag,
        last_modified: remote.validators.last_modified,
    })
}

/// Fetches a small text resource such as a playlist with the same client settings a
/// download would use. Returns the text and the URL after redirects (relative links in a
/// playlist are relative to that).
pub async fn fetch_text(
    url: &str,
    headers: &[(String, String)],
    opts: &Options,
    limit: usize,
) -> Result<(String, String)> {
    Http::new(&http_config(headers, opts))?
        .text(url, limit)
        .await
}

/// Like [`fetch_text`], for a small file that is not text (a `.torrent`).
pub async fn fetch_bytes(
    url: &str,
    headers: &[(String, String)],
    opts: &Options,
    limit: usize,
) -> Result<Vec<u8>> {
    Ok(Http::new(&http_config(headers, opts))?
        .bytes(url, limit)
        .await?
        .0)
}
