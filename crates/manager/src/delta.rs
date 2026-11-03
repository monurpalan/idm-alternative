//! Delta downloads (zsync) in the list. A `.zsync` link downloads the file it describes; a
//! plain link to a file whose older copy is here (of the same name in its folder, or an
//! earlier download of the address still on the disk) is asked for a `.zsync` next to it,
//! and with one, only the blocks that changed are downloaded.

use std::path::{Path, PathBuf};

use shardload_engine::{Options, Outcome, Progress, Request};
use tokio::sync::watch;

use super::*;

/// At most this many older copies are searched.
const MAX_SEEDS: usize = 3;

/// Whether `url` is a `.zsync` file.
pub(crate) fn is_zsync(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| u.path().to_ascii_lowercase().ends_with(".zsync"))
}

impl Inner {
    /// Older copies of the file going to `path`: of the same name in its folder (`disk (1).iso`
    /// is `disk.iso`), and earlier downloads of `url` still on the disk.
    pub(crate) async fn seeds(&self, url: &str, path: &Path) -> Vec<PathBuf> {
        let mut seeds = Vec::new();
        let wanted = path.file_name().map(|n| plain_name(&n.to_string_lossy()));
        if let (Some(dir), Some(wanted)) = (path.parent(), wanted)
            && let Ok(entries) = std::fs::read_dir(dir)
        {
            seeds.extend(
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p != path && p.is_file())
                    .filter(|p| {
                        p.file_name()
                            .is_some_and(|n| plain_name(&n.to_string_lossy()) == wanted)
                    }),
            );
        }
        for h in self.db.history_of_url(url).await.unwrap_or_default() {
            if h.path != path && h.path.is_file() && !seeds.contains(&h.path) {
                seeds.push(h.path);
            }
        }
        seeds.truncate(MAX_SEEDS);
        seeds
    }

    /// Downloads `req` as a delta when it can be one: from a `.zsync` link, or when an older
    /// copy is here and the server has a `.zsync` for the file; otherwise as usual. A note
    /// says how much came from the older copy.
    pub(crate) async fn download_maybe_delta(
        &self,
        task: &mut Task,
        req: &Request,
        opts: &Options,
        tx: &watch::Sender<Progress>,
        cancel: &CancellationToken,
    ) -> shardload_engine::Result<Outcome> {
        let explicit = is_zsync(&req.url);
        let mut part = req.output.clone().into_os_string();
        part.push(".shardload-part");
        let resuming = PathBuf::from(part).exists();
        // A delta download that stopped part way goes on as one, from where it stopped.
        let delta_resumes = shardload_engine::zsync::resumes(&req.output);
        let web = req.url.starts_with("http://") || req.url.starts_with("https://");
        if !web || (resuming && !explicit && !delta_resumes) {
            return shardload_engine::download(req, opts, Some(tx), cancel).await;
        }
        let seeds = self.seeds(&req.url, &req.output).await;
        if seeds.is_empty() && !explicit && !delta_resumes {
            return shardload_engine::download(req, opts, Some(tx), cancel).await;
        }
        let control_url = if explicit {
            req.url.clone()
        } else {
            match shardload_engine::zsync::control_url(&req.url) {
                Some(u) => u,
                None => return shardload_engine::download(req, opts, Some(tx), cancel).await,
            }
        };
        let control =
            match shardload_engine::zsync::fetch_control(&control_url, &req.headers, opts).await {
                Ok(c) => c,
                Err(e) if explicit => return Err(e),
                // No `.zsync` next to the file: the usual download.
                Err(_) => return shardload_engine::download(req, opts, Some(tx), cancel).await,
            };
        let delta = shardload_engine::zsync::update(
            &control_url,
            &control,
            &seeds,
            req,
            opts,
            Some(tx),
            cancel,
        )
        .await?;
        if let (true, Some(from)) = (delta.reused > 0, &delta.from) {
            let mb = |n: u64| format!("{:.1} MB", n as f64 / f64::from(1 << 20));
            let note = format!(
                "{} of {} came from the older copy {} (zsync): only the rest was downloaded.",
                mb(delta.reused),
                mb(control.length),
                from.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            );
            self.note(task, note).await.ok();
        }
        Ok(delta.outcome)
    }
}
