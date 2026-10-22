//! "Where did this file come from?": a file on disk traced back to its download, by its
//! place in the history, by its content, or by the mark the browser (or this app) left on
//! it; then its checksum checked again and its address asked whether there is something newer.

use std::path::{Path, PathBuf};

use shardload_engine::{Algorithm, Checksum, Options};

use crate::db::Db;
use crate::task::HistoryEntry;
use crate::{Error, Result};

/// How a file was traced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Traced {
    /// The history has a download saved at this path.
    Path,
    /// The history has a download of the same size and content, saved elsewhere.
    Content,
    /// Not in the history, but the file says where it came from (Zone.Identifier, xattr).
    Mark,
    /// Nothing says.
    Unknown,
}

/// Where a file came from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Origin {
    pub path: PathBuf,
    pub size: u64,
    pub traced: Traced,
    /// The download, when the history has it.
    pub history: Option<HistoryEntry>,
    /// The file's address.
    pub url: Option<String>,
    /// The page it was downloaded from.
    pub page: Option<String>,
    /// Unix milliseconds, when known.
    pub downloaded_at: Option<i64>,
    /// `algorithm:hex` it was checked against when it was downloaded.
    pub checksum: Option<String>,
}

/// The same path, ignoring case on Windows.
fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// Traces the file at `path`: first the history by path, then by size and SHA-256 (a
/// download's recorded checksum, or its file where it still is), then the file's own mark.
pub(crate) async fn trace(db: &Db, path: &Path) -> Result<Origin> {
    let meta = std::fs::metadata(path).map_err(shardload_engine::Error::from)?;
    if !meta.is_file() {
        return Err(Error::CannotMove(format!(
            "{} is not a file",
            path.display()
        )));
    }
    let size = meta.len();
    let candidates = db.history_of_size(size).await?;
    let by_path = candidates
        .iter()
        .find(|h| same_path(&h.path, path))
        .cloned();
    let found = match by_path {
        Some(h) => Some((h, Traced::Path)),
        None => by_content(path, candidates)
            .await?
            .map(|h| (h, Traced::Content)),
    };
    if let Some((h, traced)) = found {
        return Ok(Origin {
            path: path.to_owned(),
            size,
            traced,
            url: Some(h.final_url.clone().unwrap_or_else(|| h.url.clone())),
            page: h.page.clone(),
            downloaded_at: Some(h.finished_at),
            checksum: h.checksum.clone(),
            history: Some(h),
        });
    }
    let mark = shardload_engine::read_mark(path);
    Ok(Origin {
        path: path.to_owned(),
        size,
        traced: if mark.is_some() {
            Traced::Mark
        } else {
            Traced::Unknown
        },
        history: None,
        url: mark.as_ref().and_then(|m| m.url.clone()),
        page: mark.and_then(|m| m.referrer),
        downloaded_at: None,
        checksum: None,
    })
}

/// A file known only by its size, and its SHA-256 or name (one dropped on the window,
/// whose path the page does not get): the download in the history with that content.
pub(crate) async fn trace_content(
    db: &Db,
    size: u64,
    sha256: Option<&str>,
    name: Option<&str>,
) -> Result<Option<HistoryEntry>> {
    let candidates = db.history_of_size(size).await?;
    let wanted = sha256.and_then(|s| format!("sha256:{s}").parse::<Checksum>().ok());
    for h in &candidates {
        let recorded = h
            .checksum
            .as_deref()
            .and_then(|c| c.parse::<Checksum>().ok())
            .filter(|c| c.algorithm == Algorithm::Sha256);
        let same = match (&wanted, recorded) {
            (Some(w), Some(r)) => *w == r,
            (Some(w), None) if h.path.is_file() => {
                shardload_engine::file_checksum(&h.path, Algorithm::Sha256).await? == *w
            }
            // Without the content, the name has to say it.
            _ => name.is_some_and(|n| crate::plain_name(n) == crate::plain_name(&h.name)),
        };
        if same {
            return Ok(Some(h.clone()));
        }
    }
    Ok(None)
}

/// The download among `candidates` (all of the file's size) with the file's content: by its
/// recorded SHA-256, or by hashing its file where it still is.
async fn by_content(path: &Path, candidates: Vec<HistoryEntry>) -> Result<Option<HistoryEntry>> {
    if candidates.is_empty() {
        return Ok(None);
    }
    let own = shardload_engine::file_checksum(path, Algorithm::Sha256).await?;
    for h in candidates {
        let recorded = h
            .checksum
            .as_deref()
            .and_then(|c| c.parse::<Checksum>().ok())
            .filter(|c| c.algorithm == Algorithm::Sha256);
        let same = match recorded {
            Some(c) => c == own,
            None if h.path.is_file() && !same_path(&h.path, path) => {
                shardload_engine::file_checksum(&h.path, Algorithm::Sha256).await? == own
            }
            None => false,
        };
        if same {
            return Ok(Some(h));
        }
    }
    Ok(None)
}

/// What checking a file's checksum again found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ChecksumCheck {
    /// The checksum checked against; `None` when there is none to check.
    pub expected: Option<String>,
    /// Where it came from: the download's record, or where the publisher posts it.
    pub from: Option<String>,
    /// The file's checksum now.
    pub actual: Option<String>,
    pub matches: Option<bool>,
}

/// Checks `path` against `recorded` (from the download), or, without one, against what
/// the publisher of `url` posts now.
pub(crate) async fn check_checksum(
    path: &Path,
    recorded: Option<&str>,
    url: Option<&str>,
    opts: &Options,
) -> Result<ChecksumCheck> {
    let mut expected = recorded.and_then(|c| c.parse::<Checksum>().ok());
    let mut from = expected.as_ref().map(|_| "download".to_owned());
    if expected.is_none()
        && let Some(url) = url
    {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some((c, at)) =
            shardload_engine::find_published_checksum(url, &name, &[], opts).await?
        {
            expected = Some(c);
            from = Some(at);
        }
    }
    let Some(expected) = expected else {
        return Ok(ChecksumCheck {
            expected: None,
            from: None,
            actual: None,
            matches: None,
        });
    };
    let actual = shardload_engine::file_checksum(path, expected.algorithm).await?;
    Ok(ChecksumCheck {
        matches: Some(actual == expected),
        expected: Some(expected.to_string()),
        from,
        actual: Some(actual.to_string()),
    })
}

/// What the file's address says now, compared with the file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UpdateCheck {
    pub size: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// The address serves something else now: another size, or a version changed after the
    /// download. `None` when it cannot tell.
    pub changed: Option<bool>,
    /// A GitHub release asset: its release, and the project's newest.
    pub release: Option<String>,
    pub latest_release: Option<String>,
}

/// Asks `url` about the file again: its size and validators now, and for a GitHub release
/// asset the newest release.
pub(crate) async fn check_update(
    url: &str,
    size: u64,
    downloaded_at: Option<i64>,
    opts: &Options,
) -> Result<UpdateCheck> {
    let probe = shardload_engine::probe(url, &[], opts).await?;
    let newer = probe
        .last_modified
        .as_deref()
        .and_then(|lm| httpdate::parse_http_date(lm).ok())
        .zip(downloaded_at)
        .map(|(modified, at)| {
            let modified = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64);
            modified > at
        });
    let changed = match probe.size {
        Some(s) if s != size => Some(true),
        Some(_) => newer.or(Some(false)),
        None => newer,
    };
    let releases = shardload_engine::github_releases(url, opts)
        .await
        .ok()
        .flatten();
    Ok(UpdateCheck {
        size: probe.size,
        etag: probe.etag,
        last_modified: probe.last_modified,
        changed,
        release: releases.as_ref().map(|(r, _)| r.clone()),
        latest_release: releases.map(|(_, l)| l),
    })
}
