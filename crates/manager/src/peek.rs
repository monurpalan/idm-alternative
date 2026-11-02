//! The skeleton of a download (see [`shardload_engine::skeleton`]), read when it starts,
//! alongside it: shown while the rest downloads, so a wrong file can be stopped early.

use std::sync::Arc;

use shardload_engine::CancellationToken;

use crate::task::Task;
use crate::{Event, Inner, lock};

impl Inner {
    /// Reads the download's skeleton in the background, once, and keeps it with it.
    pub(crate) fn peek(self: &Arc<Self>, task: &Task, cancel: &CancellationToken) {
        if task.skeleton.is_some()
            || task.stream.is_some()
            || crate::torrent::is_torrent(&task.url)
            || !lock(&self.config).skeleton_first
        {
            return;
        }
        let Some(name) = task
            .name
            .clone()
            .or_else(|| shardload_engine::name::from_url(&task.url))
        else {
            return;
        };
        let mut opts = lock(&self.config).engine.clone();
        if let Some(proxy) = self.network_proxy() {
            opts.proxy = Some(proxy);
        }
        let (inner, id, url, headers) = (
            Arc::clone(self),
            task.id,
            task.url.clone(),
            task.headers.clone(),
        );
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let read = shardload_engine::skeleton::skeleton(&url, &headers, &opts, &name);
            let found = tokio::select! {
                r = read => r,
                () = cancel.cancelled() => return,
            };
            match found {
                Ok(Some(skeleton)) => {
                    if let Err(e) = inner.db.set_skeleton(id, &skeleton).await {
                        tracing::warn!(id, "could not keep the skeleton: {e}");
                    } else if let Ok(Some(t)) = inner.db.get(id).await {
                        inner.emit(Event::Updated(t));
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::debug!(id, "could not read the skeleton: {e}"),
            }
        });
    }
}
