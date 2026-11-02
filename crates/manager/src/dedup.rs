//! A file already on disk with the content about to be downloaded: found by the checksum the
//! download is expected to have (given, shown on its page, or published next to it), against
//! the SHA-256 the history keeps of every finished download. It can be copied or, on the same
//! drive, hard-linked instead of downloaded again.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use shardload_engine::{Algorithm, Checksum};

use crate::task::{Category, HistoryEntry, NewTask, Task};
use crate::{Error, Inner, Manager, Matched, Previous, Result, lock, now_ms};

/// How long looking for a checksum published next to the file may take before adding.
const PUBLISHED_WAIT: Duration = Duration::from_secs(4);
/// A file finished before its SHA-256 was kept is hashed while looking, up to this size.
const HASH_NOW_UP_TO: u64 = 256 << 20;

/// One finished file is hashed at a time, in the background.
static HASHING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// How a file already on disk is used instead of downloading it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reuse {
    Copy,
    /// A second name for the same file (a hard link): no space taken, same drive only.
    Link,
}

/// What the history keeps as a file's SHA-256: the checksum it was checked against, when
/// that is one.
pub(crate) fn sha256_of(checksum: Option<&str>) -> Option<String> {
    let c: Checksum = checksum?.parse().ok()?;
    (c.algorithm == Algorithm::Sha256).then(|| c.to_string())
}

impl Inner {
    /// Hashes a finished download's file for the history when it was not checked against a
    /// SHA-256, on a blocking thread, one file at a time.
    pub(crate) fn hash_later(self: &Arc<Self>, task: &Task) {
        let (Some(path), true) = (
            task.path.clone(),
            sha256_of(task.checksum.as_deref()).is_none(),
        ) else {
            return;
        };
        if !path.is_file() {
            return;
        }
        let inner = Arc::clone(self);
        let id = task.id;
        tokio::spawn(async move {
            let Ok(_one) = HASHING.acquire().await else {
                return;
            };
            match shardload_engine::file_checksum(&path, Algorithm::Sha256).await {
                Ok(sum) => {
                    if let Err(e) = inner
                        .db
                        .set_history_sha256(id, &path, &sum.to_string())
                        .await
                    {
                        tracing::warn!(id, "could not keep the file's SHA-256: {e}");
                    }
                }
                Err(e) => tracing::debug!(id, "could not hash the finished file: {e}"),
            }
        });
    }
}

impl Manager {
    /// A finished file of `size` bytes still on disk whose content is the one `url` (named
    /// `name`) is expected to have: by `checksum` when given, else by one published next to
    /// the file (looked for only when the history has a file of that size at all). `dir` is
    /// where the new download would go, for whether a hard link is possible.
    pub async fn find_same_content(
        &self,
        url: &str,
        name: Option<&str>,
        size: Option<u64>,
        checksum: Option<&str>,
        headers: &[(String, String)],
        dir: Option<&Path>,
    ) -> Result<Option<Previous>> {
        let Some(size) = size.filter(|s| *s > 0) else {
            return Ok(None);
        };
        let candidates: Vec<HistoryEntry> = self
            .inner
            .db
            .history_of_size(size)
            .await?
            .into_iter()
            .filter(|h| (h.sha256.is_some() || size <= HASH_NOW_UP_TO) && crate::on_disk(h))
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        let expected = match checksum.and_then(|c| c.parse::<Checksum>().ok()) {
            Some(c) => Some(c),
            None => self.published(url, name, headers).await,
        };
        let Some(expected) = expected.filter(|c| c.algorithm == Algorithm::Sha256) else {
            return Ok(None);
        };
        let want = expected.to_string();
        let mut found = None;
        for mut h in candidates {
            if h.sha256.is_none() {
                // Finished before SHA-256s were kept: hashed now (it is small), and kept.
                let sum = shardload_engine::file_checksum(&h.path, Algorithm::Sha256).await?;
                if let Some(task) = h.task_id {
                    self.inner
                        .db
                        .set_history_sha256(task, &h.path, &sum.to_string())
                        .await?;
                }
                h.sha256 = Some(sum.to_string());
            }
            if h.sha256.as_deref() == Some(&want) {
                found = Some(h);
                break;
            }
        }
        let Some(h) = found else {
            return Ok(None);
        };
        let dir = dir.map_or_else(|| self.folder_for(&h.name), Path::to_owned);
        let linkable = h.path.parent().is_some_and(|p| same_volume(p, &dir));
        let mut previous = Previous::of(h, Matched::Content, &self.inner.db).await;
        previous.linkable = linkable;
        Ok(Some(previous))
    }

    /// A checksum published for the file (GitHub's digest, a checksum file next to it),
    /// given a few seconds.
    async fn published(
        &self,
        url: &str,
        name: Option<&str>,
        headers: &[(String, String)],
    ) -> Option<Checksum> {
        let opts = lock(&self.inner.config).engine.clone();
        let name = name.map_or_else(
            || shardload_engine::name::from_url(url).unwrap_or_default(),
            str::to_owned,
        );
        let search = shardload_engine::find_published_checksum(url, &name, headers, &opts);
        match tokio::time::timeout(PUBLISHED_WAIT, search).await {
            Ok(Ok(Some((checksum, _from)))) => Some(checksum),
            _ => None,
        }
    }

    /// Where a new download named `name` goes by default.
    fn folder_for(&self, name: &str) -> PathBuf {
        let cfg = lock(&self.inner.config);
        cfg.category_dirs
            .get(&Category::for_name(name))
            .cloned()
            .unwrap_or_else(|| cfg.dir.clone())
    }

    /// Puts the file of history entry `history` into `dir` (the default folder when `None`)
    /// under `name` (its own when `None`) by copying or hard-linking it, and lists it as a
    /// finished download of its address.
    pub async fn reuse(
        &self,
        history: i64,
        dir: Option<PathBuf>,
        name: Option<String>,
        how: Reuse,
    ) -> Result<Task> {
        let db = &self.inner.db;
        let h = db
            .history_entry(history)
            .await?
            .filter(crate::on_disk)
            .ok_or_else(|| Error::CannotMove("the file is no longer where it was saved".into()))?;
        let name = name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| h.name.clone());
        let dir = dir.unwrap_or_else(|| self.folder_for(&name));
        if how == Reuse::Link && !h.path.parent().is_some_and(|p| same_volume(p, &dir)) {
            return Err(Error::CannotMove(
                "a hard link can only be made on the same drive".into(),
            ));
        }
        std::fs::create_dir_all(&dir).map_err(shardload_engine::Error::from)?;
        let taken: std::collections::HashSet<PathBuf> =
            db.paths_except(-1).await?.into_iter().collect();
        let path = shardload_engine::target_avoiding(
            &dir,
            &name,
            &h.url,
            shardload_engine::Conflict::Rename,
            |p| taken.contains(p),
        );
        let (from, to) = (h.path.clone(), path.clone());
        tokio::task::spawn_blocking(move || match how {
            Reuse::Copy => std::fs::copy(&from, &to).map(|_| ()),
            Reuse::Link => std::fs::hard_link(&from, &to),
        })
        .await
        .map_err(std::io::Error::other)
        .and_then(|r| r)
        .map_err(shardload_engine::Error::from)?;
        let file_name = path
            .file_name()
            .map_or_else(|| name.clone(), |n| n.to_string_lossy().into_owned());
        let task = self
            .add(NewTask {
                url: h.url.clone(),
                dir: Some(dir),
                name: Some(file_name.clone()),
                checksum: h.sha256.clone(),
                start: false,
                ..NewTask::default()
            })
            .await?;
        let category = Category::for_name(&file_name);
        db.set_target(task.id, &file_name, &path, h.size, category)
            .await?;
        let at = now_ms();
        db.finish(task.id, h.size.unwrap_or_default(), at).await?;
        let task = self.get(task.id).await?;
        db.record_history(&task, h.page.as_deref()).await?;
        self.inner.emit(crate::Event::Updated(task.clone()));
        Ok(task)
    }
}

/// Whether two folders are on the same drive (volume), where a hard link can join them.
pub fn same_volume(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // The folder may not exist yet: the nearest one that does.
        let dev = |p: &Path| {
            p.ancestors()
                .find_map(|a| std::fs::metadata(a).ok())
                .map(|m| m.dev())
        };
        dev(a).is_some() && dev(a) == dev(b)
    }
    #[cfg(not(unix))]
    {
        let root = |p: &Path| {
            let full = std::path::absolute(p).ok()?;
            match full.components().next()? {
                std::path::Component::Prefix(prefix) => {
                    Some(prefix.as_os_str().to_string_lossy().to_ascii_lowercase())
                }
                _ => None,
            }
        };
        root(a).is_some() && root(a) == root(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_sha256_is_kept_as_one() {
        let hex = "ab".repeat(32);
        assert_eq!(
            sha256_of(Some(&format!("SHA256:{hex}"))),
            Some(format!("sha256:{hex}"))
        );
        assert_eq!(sha256_of(Some(&format!("sha1:{}", "ab".repeat(20)))), None);
        assert_eq!(sha256_of(Some("nonsense")), None);
        assert_eq!(sha256_of(None), None);
    }

    #[test]
    fn drives() {
        let here = std::env::current_dir().expect("cwd");
        assert!(same_volume(&here, &here.join("not-yet").join("deeper")));
        #[cfg(windows)]
        {
            assert!(same_volume(Path::new(r"C:\a"), Path::new(r"c:\b\c")));
            assert!(!same_volume(Path::new(r"C:\a"), Path::new(r"D:\a")));
            assert!(!same_volume(
                Path::new(r"\\server\share\a"),
                Path::new(r"C:\a")
            ));
        }
        #[cfg(target_os = "linux")]
        assert!(!same_volume(Path::new("/proc/self"), &here));
    }
}
