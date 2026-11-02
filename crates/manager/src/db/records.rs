//! The rest of the list's tables: settings, saved logins, the sites' memory, groups, and
//! the history of finished downloads.

use super::*;

impl Db {
    /// How a finished download's actions went (see [`crate::after`]).
    pub(crate) async fn set_after_log(&self, id: TaskId, log: &[crate::after::Done]) -> Result<()> {
        sqlx::query("UPDATE tasks SET after_log = ? WHERE id = ?")
            .bind(serde_json::to_string(log)?)
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
            "INSERT INTO sites (site, max_connections, no_ranges, browser, link_lifetime, speed,
                                seen_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (site) DO UPDATE SET
                 max_connections = excluded.max_connections, no_ranges = excluded.no_ranges,
                 browser = excluded.browser, link_lifetime = excluded.link_lifetime,
                 speed = excluded.speed, seen_at = excluded.seen_at",
        )
        .bind(&m.site)
        .bind(m.max_connections.map(|v| v as i64))
        .bind(m.no_ranges)
        .bind(&m.browser)
        .bind(m.link_lifetime.map(|v| v as i64))
        .bind(m.speed.map(|v| v as i64))
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

    pub(crate) async fn new_group(&self, name: &str, at: i64) -> Result<i64> {
        Ok(
            sqlx::query("INSERT INTO groups (name, added_at) VALUES (?, ?)")
                .bind(name)
                .bind(at)
                .execute(&self.pool)
                .await?
                .last_insert_rowid(),
        )
    }

    /// Groups that still have downloads on the list, newest first.
    pub(crate) async fn groups(&self) -> Result<Vec<Group>> {
        let rows = sqlx::query(
            "SELECT g.id, g.name, g.added_at FROM groups g
             WHERE EXISTS (SELECT 1 FROM tasks t WHERE t.group_id = g.id)
             ORDER BY g.added_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Group {
                    id: r.try_get("id")?,
                    name: r.try_get("name")?,
                    added_at: r.try_get("added_at")?,
                })
            })
            .collect()
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
}

pub(crate) fn site_memory(r: &SqliteRow) -> Result<SiteMemory> {
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
        speed: r.try_get::<Option<i64>, _>("speed")?.map(|v| v as u64),
        seen_at: r.try_get("seen_at")?,
    })
}

pub(crate) fn history_entry(r: &SqliteRow) -> Result<HistoryEntry> {
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
