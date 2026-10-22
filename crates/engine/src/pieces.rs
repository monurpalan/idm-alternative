//! Piece hashes (a metalink's `<pieces>`): the file is checked piece by piece, so a piece
//! that came in corrupt is downloaded again on its own instead of the whole file.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use futures_util::StreamExt;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::checksum::{Checksum, Hasher};
use crate::download::{Phase, Progress};
use crate::error::{Error, Result};
use crate::remote::Http;

/// The hash of every `length` bytes of a file, the last piece shorter.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Pieces {
    pub length: u64,
    /// Each piece's `algorithm:hex` (see [`Checksum`]).
    pub hashes: Vec<String>,
}

impl Pieces {
    /// `[start, end)` of piece `i` in a file of `size` bytes.
    fn range(&self, i: usize, size: u64) -> (u64, u64) {
        let start = i as u64 * self.length;
        (start, (start + self.length).min(size))
    }
}

/// The pieces of `path` whose hash is not the one given (all of them when they cannot be
/// read). Also when the file is longer or shorter than the pieces say, which no piece can
/// mend: then the file is wrong as a whole.
pub(crate) async fn bad_pieces(
    path: &Path,
    pieces: &Pieces,
    cancel: &CancellationToken,
) -> Result<Vec<usize>> {
    let want: Vec<Checksum> = pieces
        .hashes
        .iter()
        .map(|h| h.parse().map_err(Error::State))
        .collect::<Result<_>>()?;
    let path = path.to_owned();
    let length = pieces.length.max(1);
    let cancel = cancel.clone();
    tokio::task::spawn_blocking(move || -> Result<Vec<usize>> {
        let mut file = std::fs::File::open(&path)?;
        let mut bad = Vec::new();
        let mut buf = vec![0u8; 1 << 20];
        for (i, want) in want.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut hasher = Hasher::new(want.algorithm);
            let mut left = length;
            while left > 0 {
                let take = left.min(buf.len() as u64) as usize;
                let n = file.read(&mut buf[..take])?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                left -= n as u64;
            }
            if hasher.finish() != want.digest {
                bad.push(i);
            }
        }
        Ok(bad)
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Checks a complete `part` of `size` bytes against `pieces` and downloads the corrupt ones
/// again from `urls` (the first that serves them), written in place; then checks again.
pub(crate) async fn repair(
    http: &Http,
    urls: &[String],
    part: &Path,
    size: u64,
    pieces: &Pieces,
    progress: Option<&watch::Sender<Progress>>,
    cancel: &CancellationToken,
) -> Result<()> {
    if pieces.hashes.len() as u64 != size.div_ceil(pieces.length.max(1)) {
        tracing::warn!(
            pieces = pieces.hashes.len(),
            size,
            "the piece hashes do not fit the file; not checking them"
        );
        return Ok(());
    }
    if let Some(p) = progress {
        p.send_modify(|p| p.phase = Phase::Verifying);
    }
    let bad = bad_pieces(part, pieces, cancel).await?;
    if bad.is_empty() {
        return Ok(());
    }
    tracing::warn!(pieces = ?bad, "corrupt pieces: downloading them again");
    for &i in &bad {
        let (start, end) = pieces.range(i, size);
        let mut last = None;
        for url in urls {
            match fetch_range(http, url, start, end, cancel).await {
                Ok(data) => {
                    let mut f = std::fs::OpenOptions::new().write(true).open(part)?;
                    f.seek(SeekFrom::Start(start))?;
                    f.write_all(&data)?;
                    last = None;
                    break;
                }
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(e) => last = Some(e),
            }
        }
        if let Some(e) = last {
            return Err(e);
        }
    }
    let still = bad_pieces(part, pieces, cancel).await?;
    if still.is_empty() {
        tracing::info!(repaired = bad.len(), "corrupt pieces downloaded again");
        Ok(())
    } else {
        Err(Error::State(format!(
            "{} of {} pieces are still corrupt after downloading them again",
            still.len(),
            pieces.hashes.len()
        )))
    }
}

async fn fetch_range(
    http: &Http,
    url: &str,
    start: u64,
    end: u64,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    let resp = tokio::select! {
        r = http.segment(url, Some((start, end)), 0) => r?,
        () = cancel.cancelled() => return Err(Error::Cancelled),
    };
    if resp.status() != wreq::StatusCode::PARTIAL_CONTENT {
        return Err(Error::RangeIgnored);
    }
    let mut data = Vec::with_capacity((end - start) as usize);
    let mut stream = resp.bytes_stream();
    loop {
        let next = tokio::select! {
            n = stream.next() => n,
            () = cancel.cancelled() => return Err(Error::Cancelled),
        };
        match next {
            Some(chunk) => data.extend_from_slice(&chunk?),
            None => break,
        }
    }
    if data.len() as u64 != end - start {
        return Err(Error::Truncated);
    }
    Ok(data)
}
