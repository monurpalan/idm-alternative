//! A link that expired, refreshed from the page the download came from: many sites put a
//! fresh signed link on the page each time it is read.

use super::*;

/// Links on the page tried at most.
const CANDIDATES: usize = 10;

impl Inner {
    /// The download's link expired: reads the page it came from (its `Referer`, with its own
    /// cookies) for another link to a file of the same name, and continues with the first that
    /// serves the same file: the same size, and the same bytes as already downloaded. Only a
    /// download with some of its file on disk, so there is something to compare. Whether it
    /// found one.
    pub(crate) async fn refresh_from_page(self: &Arc<Self>, id: TaskId) -> bool {
        match self.try_refresh(id).await {
            Ok(found) => found,
            Err(e) => {
                tracing::debug!(id, "could not look for a fresh link on the page: {e}");
                false
            }
        }
    }

    async fn try_refresh(self: &Arc<Self>, id: TaskId) -> Result<bool> {
        let Some(task) = self.db.get(id).await? else {
            return Ok(false);
        };
        let (Some(path), None) = (&task.path, &task.stream) else {
            return Ok(false);
        };
        let page = task
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("referer"))
            .map(|(_, v)| v.clone());
        let (Some(page), Some(name)) = (page, file_name(&task.url)) else {
            return Ok(false);
        };
        let opts = lock(&self.config).engine.clone();
        let (html, final_url) =
            shardload_engine::fetch_text(&page, &task.headers, &opts, grabber::PAGE_LIMIT).await?;
        let Ok(base) = url::Url::parse(&final_url) else {
            return Ok(false);
        };
        let candidates = grabber::link_urls(&html, &base)
            .into_iter()
            .filter(|u| u.as_str() != task.url && file_name(u.as_str()).as_ref() == Some(&name))
            .take(CANDIDATES);
        for candidate in candidates {
            let req = shardload_engine::Request {
                url: candidate.to_string(),
                output: path.clone(),
                headers: task.headers.clone(),
                mirrors: Vec::new(),
            };
            if let Err(e) = shardload_engine::patch_url(&req, &opts).await {
                tracing::debug!(id, %candidate, "not the same file: {e}");
                continue;
            }
            tracing::info!(id, %candidate, "expired link refreshed from its page");
            self.db.set_url(id, candidate.as_str(), now_ms()).await?;
            self.db
                .set_status(
                    id,
                    Status::Queued,
                    Some("the link expired; continuing with a fresh one from its page"),
                )
                .await?;
            if let Some(t) = self.db.get(id).await? {
                self.emit(Event::Updated(t));
            }
            self.fill().await?;
            return Ok(true);
        }
        Ok(false)
    }
}

/// The last part of a URL's path, as the file's name: `setup.exe` of `.../v2/setup.exe?sig=…`.
fn file_name(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let last = u.path_segments()?.next_back()?;
    (!last.is_empty()).then(|| last.to_owned())
}
