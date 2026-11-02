//! An earlier download of what is about to be added: on the list, or in the history.

use std::path::PathBuf;

use crate::db::Db;
use crate::task::{HistoryEntry, Status, Task, TaskId};

/// How an earlier download of the same file was found (see [`Manager::find_previous`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Matched {
    /// The same address (with its query), or the address it led to.
    Url,
    /// Another address, but a file of the same name and size.
    NameSize,
    /// The same content, by the checksum the new download is expected to have.
    Content,
}

/// An earlier download of what is about to be added.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Previous {
    pub matched: Matched,
    /// The download, while it is on the list.
    pub id: Option<TaskId>,
    /// Its status on the list; not `done` means it is still to finish ("already on the list").
    pub status: Option<Status>,
    /// Its history entry, once finished.
    pub history: Option<i64>,
    pub name: Option<String>,
    pub path: Option<PathBuf>,
    pub size: Option<u64>,
    /// Unix milliseconds.
    pub finished_at: Option<i64>,
    /// The finished file is still where it was saved, with its size.
    pub on_disk: bool,
    /// It is on the drive the new download would go to, so a hard link can stand in for it.
    pub linkable: bool,
}

impl Previous {
    /// Still to finish: queued, running, paused or failed.
    pub fn unfinished(&self) -> bool {
        self.status.is_some_and(|s| s != Status::Done)
    }

    pub(crate) fn listed(t: Task, matched: Matched) -> Self {
        Self {
            matched,
            id: Some(t.id),
            status: Some(t.status),
            history: None,
            name: t.name,
            path: t.path,
            size: t.size,
            finished_at: None,
            on_disk: false,
            linkable: false,
        }
    }

    pub(crate) async fn of(h: HistoryEntry, matched: Matched, db: &Db) -> Self {
        let status = match h.task_id {
            Some(id) => db.get(id).await.ok().flatten().map(|t| t.status),
            None => None,
        };
        Self {
            matched,
            id: h.task_id.filter(|_| status.is_some()),
            status,
            on_disk: on_disk(&h),
            history: Some(h.id),
            name: Some(h.name),
            path: Some(h.path),
            size: h.size,
            finished_at: Some(h.finished_at),
            linkable: false,
        }
    }
}

/// The history entry's file is still where it was saved, with the size it had.
pub(crate) fn on_disk(h: &HistoryEntry) -> bool {
    std::fs::metadata(&h.path).is_ok_and(|m| match h.size {
        Some(size) if m.is_file() => m.len() == size,
        _ => true,
    })
}
