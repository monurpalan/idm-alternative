//! Downloading again only what a structure check found damaged, then checking again.

use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{Format, Verdict};
use crate::download::{Phase, Progress};
use crate::error::{Error, Result};
use crate::remote::Http;

/// Rounds of downloading damaged ranges again before the file is taken as damaged at the
/// source.
const ROUNDS: usize = 2;

/// How a file without a checksum fared when checked by its structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub format: Format,
    /// Bytes downloaded again because they were damaged.
    pub repaired: u64,
    /// Still damaged after that: the file on the server is.
    pub damaged: bool,
}

/// Checks `part` (the whole download, `size` bytes) by its structure, downloading damaged
/// ranges again from the first of `urls` that sends them, and checks again. `None` when the
/// file is not of a kind it checks.
pub(crate) async fn repair(
    http: &Http,
    urls: &[String],
    part: &Path,
    progress: Option<&watch::Sender<Progress>>,
    cancel: &CancellationToken,
) -> Result<Option<Checked>> {
    if let Some(p) = progress {
        p.send_modify(|p| {
            p.phase = Phase::Verifying;
            p.speed = 0.0;
            p.connections = 0;
        });
    }
    let mut repaired = 0;
    for round in 0..=ROUNDS {
        let (format, ranges) = match check(part, cancel).await? {
            Verdict::Unknown => return Ok(None),
            Verdict::Sound(format) => {
                return Ok(Some(Checked {
                    format,
                    repaired,
                    damaged: false,
                }));
            }
            Verdict::Damaged(format, ranges) => (format, ranges),
        };
        if round == ROUNDS {
            tracing::warn!(?ranges, "still damaged after downloading those parts again");
            return Ok(Some(Checked {
                format,
                repaired,
                damaged: true,
            }));
        }
        tracing::warn!(
            format = format.name(),
            ?ranges,
            "damaged parts: downloading them again"
        );
        for &(start, end) in &ranges {
            let data = match fetch(http, urls, start, end, cancel).await {
                Ok(data) => data,
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                // The file is kept as it is; that it is damaged is said.
                Err(e) => {
                    tracing::warn!("could not download a damaged part again: {e}");
                    return Ok(Some(Checked {
                        format,
                        repaired,
                        damaged: true,
                    }));
                }
            };
            let mut f = std::fs::OpenOptions::new().write(true).open(part)?;
            f.seek(SeekFrom::Start(start))?;
            f.write_all(&data)?;
            repaired += end - start;
        }
    }
    unreachable!("the last round returns")
}

/// The check, on a blocking thread, stopped when the download is.
async fn check(part: &Path, cancel: &CancellationToken) -> Result<Verdict> {
    let stop = Arc::new(AtomicBool::new(false));
    let path = part.to_owned();
    let flag = Arc::clone(&stop);
    let job = tokio::task::spawn_blocking(move || super::check(&path, &flag));
    let stopping = cancel.clone();
    let guard = tokio::spawn(async move {
        stopping.cancelled().await;
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let result = job.await.map_err(std::io::Error::other)?;
    guard.abort();
    result
}

/// `start..end` from the first address that sends it.
async fn fetch(
    http: &Http,
    urls: &[String],
    start: u64,
    end: u64,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    let mut last = Error::State("no address to download it from".into());
    for url in urls {
        match crate::pieces::fetch_range(http, url, start, end, cancel).await {
            Ok(data) => return Ok(data),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(e) => last = e,
        }
    }
    Err(last)
}
