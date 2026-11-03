//! The Wayback Machine, for a file its server no longer has (see [`ErrorKind::Missing`]). Only
//! ever at the user's click: the search page opens in the browser, and "add the archived copy"
//! asks the Wayback Machine once for its closest copy of the address and adds that, as the
//! bytes it archived (`id_`), as a new download. Nothing is asked by itself.

use serde::Serialize;

use crate::task::{NewTask, TaskId};
use crate::{Manager, Result};

/// The Wayback Machine's list of the copies it has of `url`.
pub fn search_url(url: &str) -> String {
    format!("https://web.archive.org/web/*/{url}")
}

/// Where the Wayback Machine says which copy of `url` is the closest.
fn available_url(url: &str) -> String {
    let query: String = url::form_urlencoded::byte_serialize(url.as_bytes()).collect();
    format!("https://archive.org/wayback/available?url={query}")
}

/// The timestamp of the archived copy in the Wayback Machine's answer, when it has one that
/// was a success (`200`).
fn closest(answer: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(answer).ok()?;
    let closest = &v["archived_snapshots"]["closest"];
    let ok = closest["available"].as_bool() == Some(true)
        && closest["status"].as_str().is_none_or(|s| s == "200");
    let ts = closest["timestamp"].as_str()?;
    (ok && ts.len() >= 8 && ts.bytes().all(|b| b.is_ascii_digit())).then(|| ts.to_owned())
}

/// The archived bytes of `url` at `timestamp`, without the Wayback Machine's toolbar or
/// rewritten links (`id_`).
fn archived_url(timestamp: &str, url: &str) -> String {
    format!("https://web.archive.org/web/{timestamp}id_/{url}")
}

/// An archived copy added to the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchivedCopy {
    pub id: TaskId,
    /// When it was archived: `YYYYMMDDhhmmss`.
    pub timestamp: String,
}

impl Manager {
    /// Asks the Wayback Machine for its closest copy of download `id`'s address and adds it as
    /// a new download (same folder and name); `None` when it has none.
    pub async fn add_archived_copy(&self, id: TaskId) -> Result<Option<ArchivedCopy>> {
        let task = self.get(id).await?;
        let opts = self.config().engine;
        let (answer, _) =
            shardload_engine::fetch_text(&available_url(&task.url), &[], &opts, 1 << 20).await?;
        let Some(timestamp) = closest(&answer) else {
            return Ok(None);
        };
        let added = self
            .add(NewTask {
                url: archived_url(&timestamp, &task.url),
                dir: Some(task.dir.clone()),
                name: task.name.clone(),
                start: true,
                ..NewTask::default()
            })
            .await?;
        Ok(Some(ArchivedCopy {
            id: added.id,
            timestamp,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let url = "https://example.com/files/a b.iso?x=1&y=2";
        assert_eq!(
            search_url(url),
            format!("https://web.archive.org/web/*/{url}")
        );
        assert_eq!(
            available_url(url),
            "https://archive.org/wayback/available?url=https%3A%2F%2Fexample.com%2Ffiles%2Fa+b.iso%3Fx%3D1%26y%3D2"
        );
        assert_eq!(
            archived_url("20240101120000", "https://example.com/a.iso"),
            "https://web.archive.org/web/20240101120000id_/https://example.com/a.iso"
        );
    }

    #[test]
    fn the_closest_copy() {
        let found = r#"{"url": "example.com/a.iso", "archived_snapshots": {"closest": {"status": "200", "available": true, "url": "http://web.archive.org/web/20240101120000/https://example.com/a.iso", "timestamp": "20240101120000"}}}"#;
        assert_eq!(closest(found).as_deref(), Some("20240101120000"));
        assert_eq!(closest(r#"{"url": "x", "archived_snapshots": {}}"#), None);
        let broken = found.replace("\"200\"", "\"404\"");
        assert_eq!(closest(&broken), None);
        let odd = found.replace("20240101120000\"}", "../x\"}");
        assert_eq!(closest(&odd), None);
        assert_eq!(closest("<html>"), None);
    }
}
