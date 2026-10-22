//! The task list in SQLite (WAL). Only the list, not live progress (ADR 0001).

use std::path::{Path, PathBuf};
use std::str::FromStr;

use sqlx::Row;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqliteRow, SqliteSynchronous,
};

use crate::Result;
use crate::sites::SiteMemory;
use crate::task::{Category, ErrorKind, HistoryEntry, Status, Stream, Task, TaskId};

#[derive(Clone)]
pub(crate) struct Db {
    pool: SqlitePool,
}

/// A task about to be inserted.
pub(crate) struct Insert<'a> {
    pub url: &'a str,
    pub headers: &'a [(String, String)],
    pub dir: &'a Path,
    pub name: Option<&'a str>,
    pub status: Status,
    pub category: Category,
    pub connections: usize,
    pub speed_limit: Option<u64>,
    pub checksum: Option<&'a str>,
    pub added_at: i64,
    pub stream: Option<&'a Stream>,
    pub mirrors: &'a [String],
}

impl Db {
    pub(crate) async fn open(path: &Path) -> Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite:")?
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // WAL + NORMAL: a crash can lose the last moments, never corrupt the list.
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        let db = Self { pool };
        db.seal_plain_headers().await?;
        Ok(db)
    }

    /// Lists from before headers were encrypted keep cookies and passwords in plain text;
    /// they are encrypted on the first open.
    async fn seal_plain_headers(&self) -> Result<()> {
        let rows = sqlx::query("SELECT id, headers FROM tasks WHERE headers != '[]'")
            .fetch_all(&self.pool)
            .await?;
        for r in rows {
            let stored: String = r.try_get("headers")?;
            if crate::secret::is_sealed(&stored) {
                continue;
            }
            sqlx::query("UPDATE tasks SET headers = ? WHERE id = ?")
                .bind(crate::secret::seal(&stored))
                .bind(r.try_get::<i64, _>("id")?)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn close(&self) {
        self.pool.close().await;
    }

    pub(crate) async fn insert(&self, t: Insert<'_>) -> Result<TaskId> {
        let headers = store_headers(t.headers)?;
        let id = sqlx::query(
            "INSERT INTO tasks (url, headers, dir, name, status, category, connections,
                                speed_limit, checksum, added_at, stream, mirrors, queue_pos)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                     (SELECT COALESCE(MAX(queue_pos), 0) + 1 FROM tasks))",
        )
        .bind(t.url)
        .bind(headers)
        .bind(t.dir.to_string_lossy())
        .bind(t.name)
        .bind(t.status.as_str())
        .bind(t.category.as_str())
        .bind(t.connections as i64)
        .bind(t.speed_limit.map(|v| v as i64))
        .bind(t.checksum)
        .bind(t.added_at)
        .bind(t.stream.map(serde_json::to_string).transpose()?)
        .bind(
            (!t.mirrors.is_empty())
                .then(|| serde_json::to_string(t.mirrors))
                .transpose()?,
        )
        .execute(&self.pool)
        .await?
        .last_insert_rowid();
        Ok(id)
    }

    pub(crate) async fn get(&self, id: TaskId) -> Result<Option<Task>> {
        let row = sqlx::query("SELECT * FROM tasks WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| task(&r)).transpose()
    }

    /// Every task, in queue order.
    pub(crate) async fn list(&self) -> Result<Vec<Task>> {
        sqlx::query("SELECT * FROM tasks ORDER BY queue_pos")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(task)
            .collect()
    }

    /// Queued tasks in the order they should start.
    pub(crate) async fn queued(&self, limit: usize) -> Result<Vec<Task>> {
        sqlx::query("SELECT * FROM tasks WHERE status = 'queued' ORDER BY queue_pos LIMIT ?")
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(task)
            .collect()
    }

    /// Also clears any recorded failure kind.
    pub(crate) async fn set_status(
        &self,
        id: TaskId,
        status: Status,
        error: Option<&str>,
    ) -> Result<()> {
        self.set_failure(id, status, error, None).await
    }

    pub(crate) async fn set_failure(
        &self,
        id: TaskId,
        status: Status,
        error: Option<&str>,
        kind: Option<ErrorKind>,
    ) -> Result<()> {
        sqlx::query("UPDATE tasks SET status = ?, error = ?, error_kind = ? WHERE id = ?")
            .bind(status.as_str())
            .bind(error)
            .bind(kind.map(ErrorKind::as_str))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(crate) async fn set_headers(&self, id: TaskId, headers: &[(String, String)]) -> Result<()> {
        sqlx::query("UPDATE tasks SET headers = ? WHERE id = ?")
            .bind(store_headers(headers)?)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Tasks a previous run left downloading (it was closed or killed) go back in the queue.
    pub(crate) async fn requeue_interrupted(&self) -> Result<u64> {
        Ok(
            sqlx::query("UPDATE tasks SET status = 'queued' WHERE status = 'downloading'")
                .execute(&self.pool)
                .await?
                .rows_affected(),
        )
    }

    pub(crate) async fn set_target(
        &self,
        id: TaskId,
        name: &str,
        path: &Path,
        size: Option<u64>,
        category: Category,
    ) -> Result<()> {
        sqlx::query("UPDATE tasks SET name = ?, path = ?, size = ?, category = ? WHERE id = ?")
            .bind(name)
            .bind(path.to_string_lossy())
            .bind(size.map(|v| v as i64))
            .bind(category.as_str())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Where a download goes, after a rename or a move.
    pub(crate) async fn set_place(
        &self,
        id: TaskId,
        dir: &Path,
        name: Option<&str>,
        path: Option<&Path>,
        category: Category,
    ) -> Result<()> {
        sqlx::query("UPDATE tasks SET dir = ?, name = ?, path = ?, category = ? WHERE id = ?")
            .bind(dir.to_string_lossy())
            .bind(name)
            .bind(path.map(|p| p.to_string_lossy()))
            .bind(category.as_str())
            .bind(id)
            .execute(&self.pool)
            .await?;
        if let (Some(name), Some(path)) = (name, path) {
            sqlx::query("UPDATE history SET name = ?, path = ? WHERE task_id = ?")
                .bind(name)
                .bind(path.to_string_lossy())
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// A checksum found for a download the user gave none for, and where it was found.
    pub(crate) async fn set_found_checksum(
        &self,
        id: TaskId,
        checksum: &str,
        from: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE tasks SET checksum = ?, checksum_from = ? WHERE id = ? AND checksum IS NULL",
        )
        .bind(checksum)
        .bind(from)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(crate) async fn set_stream(&self, id: TaskId, stream: &Stream) -> Result<()> {
        sqlx::query("UPDATE tasks SET stream = ? WHERE id = ?")
            .bind(serde_json::to_string(stream)?)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Output paths given to tasks other than `except`.
    pub(crate) async fn paths_except(&self, except: TaskId) -> Result<Vec<PathBuf>> {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT path FROM tasks WHERE path IS NOT NULL AND id != ?")
                .bind(except)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(PathBuf::from).collect())
    }

    pub(crate) async fn set_received(
        &self,
        id: TaskId,
        received: u64,
        size: Option<u64>,
    ) -> Result<()> {
        sqlx::query("UPDATE tasks SET received = ?, size = COALESCE(?, size) WHERE id = ?")
            .bind(received as i64)
            .bind(size.map(|v| v as i64))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(crate) async fn finish(&self, id: TaskId, size: u64, at: i64) -> Result<()> {
        sqlx::query(
            "UPDATE tasks SET status = 'done', error = NULL, error_kind = NULL, size = ?, received = ?,
                              finished_at = ? WHERE id = ?",
        )
        .bind(size as i64)
        .bind(size as i64)
        .bind(at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(crate) async fn set_speed_limit(&self, id: TaskId, limit: Option<u64>) -> Result<()> {
        sqlx::query("UPDATE tasks SET speed_limit = ? WHERE id = ?")
            .bind(limit.map(|v| v as i64))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(crate) async fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub(crate) async fn save_setting(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, ?)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(crate) async fn save_login(&self, site: &str, user: &str, password: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO logins (site, user, password, saved_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (site) DO UPDATE
             SET user = excluded.user, password = excluded.password, saved_at = excluded.saved_at",
        )
        .bind(site)
        .bind(user)
        .bind(crate::secret::seal(password))
        .bind(crate::now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The login saved for `site`; none also when its password cannot be decrypted.
    pub(crate) async fn login(&self, site: &str) -> Result<Option<(String, String)>> {
        let row = sqlx::query("SELECT user, password FROM logins WHERE site = ?")
            .bind(site)
            .fetch_optional(&self.pool)
            .await?;
        let Some(r) = row else { return Ok(None) };
        let user: String = r.try_get("user")?;
        let stored: String = r.try_get("password")?;
        Ok(crate::secret::open(&stored).map(|password| (user, password)))
    }

    /// Saved sites and user names, newest first; never the passwords.
    pub(crate) async fn logins(&self) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query("SELECT site, user FROM logins ORDER BY saved_at DESC")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("site")?, r.try_get("user")?)))
            .collect()
    }

    pub(crate) async fn forget_login(&self, site: &str) -> Result<()> {
        sqlx::query("DELETE FROM logins WHERE site = ?")
            .bind(site)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(crate) async fn site(&self, site: &str) -> Result<Option<SiteMemory>> {
        sqlx::query("SELECT * FROM sites WHERE site = ?")
            .bind(site)
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(site_memory)
            .transpose()
    }

    /// Every remembered site, most recently seen first.
    pub(crate) async fn sites(&self) -> Result<Vec<SiteMemory>> {
        sqlx::query("SELECT * FROM sites ORDER BY seen_at DESC")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(site_memory)
            .collect()
    }

    pub(crate) async fn save_site(&self, m: &SiteMemory) -> Result<()> {
        sqlx::query(
            "INSERT INTO sites (site, max_connections, no_ranges, browser, link_lifetime, seen_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (site) DO UPDATE SET
                 max_connections = excluded.max_connections, no_ranges = excluded.no_ranges,
                 browser = excluded.browser, link_lifetime = excluded.link_lifetime,
                 seen_at = excluded.seen_at",
        )
        .bind(&m.site)
        .bind(m.max_connections.map(|v| v as i64))
        .bind(m.no_ranges)
        .bind(&m.browser)
        .bind(m.link_lifetime.map(|v| v as i64))
        .bind(m.seen_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(crate) async fn forget_site(&self, site: &str) -> Result<()> {
        sqlx::query("DELETE FROM sites WHERE site = ?")
            .bind(site)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(crate) async fn set_queue_pos(&self, id: TaskId, pos: i64) -> Result<()> {
        sqlx::query("UPDATE tasks SET queue_pos = ? WHERE id = ?")
            .bind(pos)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(crate) async fn set_url(&self, id: TaskId, url: &str) -> Result<()> {
        sqlx::query("UPDATE tasks SET url = ? WHERE id = ?")
            .bind(url)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Back to the queue with nothing downloaded, keeping the chosen path.
    pub(crate) async fn reset(&self, id: TaskId) -> Result<()> {
        sqlx::query(
            "UPDATE tasks SET status = 'queued', error = NULL, error_kind = NULL, received = 0,
                              finished_at = NULL
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        // Its file is gone; the history gets it again when it finishes.
        sqlx::query("DELETE FROM history WHERE task_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Its history entry stays, no longer tied to the list.
    pub(crate) async fn delete(&self, id: TaskId) -> Result<()> {
        sqlx::query("DELETE FROM tasks WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("UPDATE history SET task_id = NULL WHERE task_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The address after redirects, learned on the first start.
    pub(crate) async fn set_final_url(&self, id: TaskId, url: &str) -> Result<()> {
        sqlx::query("UPDATE tasks SET final_url = ? WHERE id = ?")
            .bind(url)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Keeps a finished download in the history, replacing what an earlier finish of the
    /// same download left there.
    pub(crate) async fn record_history(&self, task: &Task, page: Option<&str>) -> Result<()> {
        let (Some(name), Some(path)) = (&task.name, &task.path) else {
            return Ok(());
        };
        sqlx::query("DELETE FROM history WHERE task_id = ?")
            .bind(task.id)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "INSERT INTO history (task_id, url, final_url, page, name, path, size, checksum,
                                  finished_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(task.id)
        .bind(&task.url)
        .bind(&task.final_url)
        .bind(page)
        .bind(name)
        .bind(path.to_string_lossy())
        .bind(task.size.map(|v| v as i64))
        .bind(&task.checksum)
        .bind(task.finished_at.unwrap_or_else(crate::now_ms))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// History entries of `url` (as given, or where it led), newest first.
    pub(crate) async fn history_of_url(&self, url: &str) -> Result<Vec<HistoryEntry>> {
        sqlx::query(
            "SELECT * FROM history WHERE url = ? OR final_url = ? ORDER BY finished_at DESC",
        )
        .bind(url)
        .bind(url)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(history_entry)
        .collect()
    }

    /// History entries of files of this size, newest first.
    pub(crate) async fn history_of_size(&self, size: u64) -> Result<Vec<HistoryEntry>> {
        sqlx::query("SELECT * FROM history WHERE size = ? ORDER BY finished_at DESC")
            .bind(size as i64)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(history_entry)
            .collect()
    }

    pub(crate) async fn history_entry(&self, id: i64) -> Result<Option<HistoryEntry>> {
        sqlx::query("SELECT * FROM history WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(history_entry)
            .transpose()
    }

    /// The whole history, newest first.
    pub(crate) async fn history(&self) -> Result<Vec<HistoryEntry>> {
        sqlx::query("SELECT * FROM history ORDER BY finished_at DESC")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(history_entry)
            .collect()
    }

    pub(crate) async fn clear_history(&self) -> Result<()> {
        sqlx::query("DELETE FROM history")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Unfinished downloads on the list for `url` (as given, or where it led).
    pub(crate) async fn unfinished_of_url(&self, url: &str) -> Result<Vec<Task>> {
        sqlx::query(
            "SELECT * FROM tasks WHERE status != 'done' AND (url = ? OR final_url = ?)
             ORDER BY queue_pos",
        )
        .bind(url)
        .bind(url)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(task)
        .collect()
    }
}

/// Headers as stored: encrypted (see `secret`), except an empty list.
fn store_headers(headers: &[(String, String)]) -> Result<String> {
    if headers.is_empty() {
        return Ok("[]".to_owned());
    }
    Ok(crate::secret::seal(&serde_json::to_string(headers)?))
}

/// Headers that cannot be decrypted (a list copied from another user or machine) are
/// dropped: the download goes on without its cookies and may ask to log in again.
fn load_headers(stored: &str) -> Result<Vec<(String, String)>> {
    match crate::secret::open(stored) {
        Some(plain) => Ok(serde_json::from_str(&plain)?),
        None => {
            tracing::warn!("could not decrypt a download's request headers; dropped");
            Ok(Vec::new())
        }
    }
}

fn task(r: &SqliteRow) -> Result<Task> {
    let u64_opt = |col: &str| -> Result<Option<u64>> {
        Ok(r.try_get::<Option<i64>, _>(col)?.map(|v| v as u64))
    };
    Ok(Task {
        id: r.try_get("id")?,
        url: r.try_get("url")?,
        headers: load_headers(r.try_get("headers")?)?,
        dir: PathBuf::from(r.try_get::<String, _>("dir")?),
        name: r.try_get("name")?,
        path: r.try_get::<Option<String>, _>("path")?.map(PathBuf::from),
        size: u64_opt("size")?,
        received: r.try_get::<i64, _>("received")? as u64,
        status: Status::parse(r.try_get("status")?),
        error: r.try_get("error")?,
        error_kind: r
            .try_get::<Option<String>, _>("error_kind")?
            .as_deref()
            .and_then(ErrorKind::parse),
        category: Category::parse(r.try_get("category")?),
        connections: r.try_get::<i64, _>("connections")? as usize,
        speed_limit: u64_opt("speed_limit")?,
        checksum: r.try_get("checksum")?,
        checksum_from: r.try_get("checksum_from")?,
        added_at: r.try_get("added_at")?,
        finished_at: r.try_get("finished_at")?,
        queue_pos: r.try_get("queue_pos")?,
        stream: r
            .try_get::<Option<String>, _>("stream")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        mirrors: r
            .try_get::<Option<String>, _>("mirrors")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or_default(),
        final_url: r.try_get("final_url")?,
    })
}

fn site_memory(r: &SqliteRow) -> Result<SiteMemory> {
    Ok(SiteMemory {
        site: r.try_get("site")?,
        max_connections: r
            .try_get::<Option<i64>, _>("max_connections")?
            .map(|v| v as usize),
        no_ranges: r.try_get("no_ranges")?,
        browser: r.try_get("browser")?,
        link_lifetime: r
            .try_get::<Option<i64>, _>("link_lifetime")?
            .map(|v| v as u64),
        seen_at: r.try_get("seen_at")?,
    })
}

fn history_entry(r: &SqliteRow) -> Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: r.try_get("id")?,
        task_id: r.try_get("task_id")?,
        url: r.try_get("url")?,
        final_url: r.try_get("final_url")?,
        page: r.try_get("page")?,
        name: r.try_get("name")?,
        path: PathBuf::from(r.try_get::<String, _>("path")?),
        size: r.try_get::<Option<i64>, _>("size")?.map(|v| v as u64),
        checksum: r.try_get("checksum")?,
        finished_at: r.try_get("finished_at")?,
    })
}
