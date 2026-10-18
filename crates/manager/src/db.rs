//! The task list in SQLite (WAL). Only the list, not live progress (ADR 0001).

use std::path::{Path, PathBuf};
use std::str::FromStr;

use sqlx::Row;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqliteRow, SqliteSynchronous,
};

use crate::Result;
use crate::task::{Category, ErrorKind, Status, Stream, Task, TaskId};

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
        Ok(())
    }

    pub(crate) async fn delete(&self, id: TaskId) -> Result<()> {
        sqlx::query("DELETE FROM tasks WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
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
    })
}
