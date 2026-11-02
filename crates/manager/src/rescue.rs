//! Continuing a download the browser left interrupted: its partial file (`.crdownload` in
//! Chromium, `.part` in Firefox, next to where it was going) is taken over when the server
//! still serves the same file, so only the rest is downloaded; otherwise it starts over.

use std::path::{Path, PathBuf};

use shardload_engine::Adopted;

use crate::task::Category;
use crate::{Manager, NewTask, Result, Task};

/// A download the browser left interrupted.
#[derive(Debug, Clone, Default)]
pub struct Rescue {
    /// The address, the browser's cookies and referrer.
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// Where the browser was saving it: the full path.
    pub target: PathBuf,
    /// The bytes the browser had; `u64::MAX`: all that is in the partial file.
    pub received: u64,
    /// The size the browser was told, if any.
    pub size: Option<u64>,
}

/// What a rescue came to.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Rescued {
    pub task: Task,
    /// Bytes of the browser's partial file kept.
    pub kept: u64,
    /// Why nothing was kept, when nothing was.
    pub why: Option<String>,
}

/// The browser's partial file for `target`, if it is there.
pub fn partial_of(target: &Path) -> Option<PathBuf> {
    [".crdownload", ".part"]
        .iter()
        .map(|ext| crate::relocate::with_suffix(target, ext))
        .find(|p| p.is_file())
}

impl Manager {
    /// Adds the interrupted download and starts it, taking over the browser's partial file
    /// when it can. The download goes where the browser was saving it only when the
    /// partial file is there (the browser was writing to that folder); otherwise into the
    /// download folder, from the start.
    pub async fn rescue(&self, r: Rescue) -> Result<Rescued> {
        let name = r
            .target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty());
        let partial = partial_of(&r.target).filter(|_| !r.target.exists());
        let task = self
            .add(NewTask {
                url: r.url.clone(),
                headers: r.headers.clone(),
                dir: partial.as_ref().and(r.target.parent().map(Path::to_owned)),
                name: name.clone(),
                size: r.size,
                start: false,
                ..NewTask::default()
            })
            .await?;
        let (kept, why) = match (partial, name) {
            (Some(partial), Some(name)) => self.adopt(&task, &partial, &name, &r).await,
            _ => (0, Some("the browser's partial file is not there".into())),
        };
        match &why {
            None => tracing::info!(id = task.id, kept, "took over the browser's partial file"),
            Some(why) => tracing::info!(
                id = task.id,
                "the browser's partial file is not used: {why}"
            ),
        }
        self.start(task.id).await?;
        Ok(Rescued {
            task: self.get(task.id).await?,
            kept,
            why,
        })
    }

    /// Takes over the partial file for `task` (not started yet), under the browser's name.
    async fn adopt(
        &self,
        task: &Task,
        partial: &Path,
        name: &str,
        r: &Rescue,
    ) -> (u64, Option<String>) {
        let path = task.dir.join(name);
        let taken = self
            .inner
            .db
            .paths_except(task.id)
            .await
            .unwrap_or_default();
        if taken.contains(&path) {
            return (0, Some("another download on the list goes there".into()));
        }
        let req = shardload_engine::Request {
            url: task.url.clone(),
            output: path.clone(),
            headers: task.headers.clone(),
            mirrors: Vec::new(),
        };
        let opts = crate::lock(&self.inner.config).engine.clone();
        let adopted = shardload_engine::adopt_partial(&req, partial, r.received, r.size, &opts)
            .await
            .map_err(|e| e.to_string());
        let (bytes, size) = match adopted {
            Ok(Adopted::Kept { bytes, size }) => (bytes, size),
            Ok(Adopted::StartOver(why)) => {
                // It holds a file the server no longer serves; the browser's entry goes too.
                std::fs::remove_file(partial).ok();
                return (0, Some(why));
            }
            Err(e) => return (0, Some(e)),
        };
        let category = Category::for_name(name);
        let set = self
            .inner
            .db
            .set_target(task.id, name, &path, Some(size), category);
        if let Err(e) = set.await {
            return (0, Some(e.to_string()));
        }
        (bytes, None)
    }
}
