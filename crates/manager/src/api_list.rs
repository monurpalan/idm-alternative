//! Opening the list, adding downloads, and what is known about them: groups, the
//! history, where a file came from.

use super::*;

impl Manager {
    /// Opens (or creates) the list at `db_path`. Downloads that were running when the list
    /// was last closed are queued again, and queued downloads start.
    pub async fn open(db_path: &Path, config: Config) -> Result<Self> {
        let db = Db::open(db_path).await?;
        let requeued = db.requeue_interrupted().await?;
        if requeued > 0 {
            tracing::info!(requeued, "resuming downloads interrupted by the last close");
        }
        let torrents = torrent::Torrents::new(config.torrent.clone(), config.dir.clone());
        let inner = Arc::new(Inner {
            torrents,
            db,
            events: broadcast::channel(1024).0,
            global: Limiter::new(None),
            config: Mutex::new(config),
            running: Mutex::new(HashMap::new()),
            scheduling: tokio::sync::Mutex::new(()),
            naming: tokio::sync::Mutex::new(()),
            retries: Mutex::new(HashMap::new()),
            moving: Mutex::new(HashSet::new()),
            closed: AtomicBool::new(false),
            notes: Mutex::new(HashMap::new()),
            later: Mutex::new(HashMap::new()),
            net: Mutex::new(net::NetState::default()),
            deadlines: Mutex::new(deadline::DeadlineState::default()),
            speeds: speeds::Live::default(),
        });
        inner.load_usage().await?;
        inner.watch_network();
        inner.watch_deadlines_often();
        for t in inner.db.list().await? {
            if t.status == Status::Queued {
                inner.start_later(&t);
            }
        }
        inner.fill().await?;
        // Looks at the drives while downloads run; ends with the manager.
        let weak = Arc::downgrade(&inner);
        let every = lock(&inner.config).disk_watch;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(inner) = weak.upgrade() else { return };
                if inner.closed.load(Ordering::SeqCst) {
                    return;
                }
                if let Err(e) = inner.watch_disks().await {
                    tracing::warn!("could not look at the free disk space: {e}");
                }
            }
        });
        Ok(Self { inner })
    }

    /// What `ids` (and the downloads running now, which write to the same drives) still
    /// need on each drive, against the space free there: for asking before a queue starts.
    pub async fn disk_needs(&self, ids: &[TaskId]) -> Result<Vec<disk::DriveNeed>> {
        let tasks: Vec<Task> = self
            .list()
            .await?
            .into_iter()
            .filter(|t| ids.contains(&t.id) || t.status == Status::Downloading)
            .collect();
        Ok(disk::needs(&tasks, &self.inner.free_space()))
    }

    /// Events for every change from now on. A receiver that falls far behind skips events
    /// (`RecvError::Lagged`) and should reload the list.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    /// A value saved with [`Manager::save_setting`], or `None` if there is none (or it no
    /// longer parses as `T`).
    pub async fn setting<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        Ok(self
            .inner
            .db
            .setting(key)
            .await?
            .and_then(|v| serde_json::from_str(&v).ok()))
    }

    pub async fn save_setting<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.inner
            .db
            .save_setting(key, &serde_json::to_string(value)?)
            .await
    }

    /// Reads a setting before opening the manager, e.g. to build its [`Config`].
    pub async fn read_setting<T: serde::de::DeserializeOwned>(
        db_path: &Path,
        key: &str,
    ) -> Result<Option<T>> {
        let db = Db::open(db_path).await?;
        let value = db.setting(key).await;
        db.close().await;
        Ok(value?.and_then(|v| serde_json::from_str(&v).ok()))
    }

    pub async fn list(&self) -> Result<Vec<Task>> {
        self.inner.db.list().await
    }

    /// How fast a download went from its first byte on, for a graph; a running one's up to
    /// now. `None` for one that never began downloading.
    pub async fn speed_history(&self, id: TaskId) -> Result<Option<SpeedHistory>> {
        if let Some(h) = lock(&self.inner.speeds).get(&id) {
            return Ok(Some(h.clone()));
        }
        self.inner.db.speeds(id).await
    }

    pub async fn get(&self, id: TaskId) -> Result<Task> {
        self.inner.db.get(id).await?.ok_or(Error::NotFound(id))
    }

    pub fn config(&self) -> Config {
        lock(&self.inner.config).clone()
    }

    /// The folders downloads are saved into: the download folder and each category's folder.
    pub fn download_folders(&self) -> Vec<PathBuf> {
        let cfg = self.config();
        std::iter::once(cfg.dir)
            .chain(cfg.category_dirs.into_values())
            .collect()
    }

    /// Applies to downloads started from now on; a larger `max_active` starts queued ones.
    pub async fn set_config(&self, config: Config) -> Result<()> {
        *lock(&self.inner.config) = config;
        self.inner.fill().await
    }

    pub async fn add(&self, mut new: NewTask) -> Result<Task> {
        // A metalink: the file it describes, from its addresses, checked against its hash.
        if shardload_engine::metalink::is_metalink(&new.url) {
            let opts = lock(&self.inner.config).engine.clone();
            let m =
                shardload_engine::metalink::fetch_metalink(&new.url, &new.headers, &opts).await?;
            let mut urls = m
                .urls
                .into_iter()
                .take(shardload_engine::metalink::MAX_URLS);
            new.url = urls.next().unwrap_or_default();
            new.mirrors = urls.chain(std::mem::take(&mut new.mirrors)).collect();
            new.name = new.name.or(Some(m.name));
            new.checksum = new.checksum.or(m.checksum);
            new.pieces = new.pieces.or(m.pieces);
        }
        // A torrent of the same file: its piece hashes check the HTTP download.
        if let Some(source) = new.pieces_from.take() {
            let opts = lock(&self.inner.config).engine.clone();
            let bytes = torrent::read_torrent_file(&source, &new.headers, &opts)
                .await
                .map_err(Error::BadTorrent)?;
            let (pieces, name, _) =
                torrent::single_file_pieces(&bytes).map_err(Error::BadTorrent)?;
            new.pieces = Some(pieces);
            new.name = new.name.or((!name.is_empty()).then_some(name));
        }
        if let Some(c) = &new.checksum {
            c.parse::<Checksum>().map_err(Error::BadChecksum)?;
        }
        // Read off a page, so never a reason to refuse the download.
        let page_checksum = new
            .page_checksum
            .take()
            .filter(|c| match c.parse::<Checksum>() {
                Ok(_) => true,
                Err(e) => {
                    tracing::warn!("left out a checksum from the page: {e}");
                    false
                }
            });
        if let Some(c) = new.stream.as_ref().and_then(|s| s.clip.as_ref()) {
            clip::check(c, None).map_err(Error::BadClip)?;
        }
        let name = new.name.as_deref().map(shardload_engine::name::sanitize);
        // A guess from the URL until the server names the file.
        let category = Category::for_name(
            &name
                .clone()
                .or_else(|| shardload_engine::name::from_url(&new.url))
                .unwrap_or_default(),
        );
        let (dir, connections) = {
            let cfg = lock(&self.inner.config);
            (
                new.dir.clone().unwrap_or_else(|| {
                    cfg.category_dirs.get(&category).unwrap_or(&cfg.dir).clone()
                }),
                new.connections.unwrap_or(cfg.connections),
            )
        };
        let id = self
            .inner
            .db
            .insert(Insert {
                url: &new.url,
                headers: &new.headers,
                dir: &dir,
                name: name.as_deref(),
                status: if new.start {
                    Status::Queued
                } else {
                    Status::Paused
                },
                category,
                connections: connections.max(1),
                speed_limit: new.speed_limit,
                checksum: new.checksum.as_deref(),
                added_at: now_ms(),
                stream: new.stream.as_ref(),
                mirrors: &new.mirrors,
                group: new.group,
                pieces: new.pieces.as_ref(),
                page_title: new.title.as_deref(),
                page_checksum: page_checksum.as_deref(),
                size: new.size,
                after: new.after.as_ref(),
                deadline: new.deadline,
            })
            .await?;
        let task = self.get(id).await?;
        self.inner.emit(Event::Added(task.clone()));
        if new.start {
            self.inner.start_later(&task);
            self.inner.fill().await?;
        }
        Ok(task)
    }

    /// An earlier download of the same file, before adding it again: first a download of
    /// `url` (or of `final_url`, where it led) that is still on the list unfinished, then
    /// one in the history (which keeps finished downloads after they leave the list), then,
    /// with `name` and `size` (known once the server was asked), a finished file of the
    /// same name and size from another address. Among several, one whose file is still there.
    pub async fn find_previous(
        &self,
        url: &str,
        final_url: Option<&str>,
        name: Option<&str>,
        size: Option<u64>,
    ) -> Result<Option<Previous>> {
        let db = &self.inner.db;
        let mut urls = vec![without_fragment(url)];
        if let Some(f) = final_url
            .map(without_fragment)
            .filter(|f| !urls.contains(f))
        {
            urls.push(f);
        }
        for u in &urls {
            if let Some(t) = db.unfinished_of_url(u).await?.into_iter().next() {
                return Ok(Some(Previous::listed(t, Matched::Url)));
            }
        }
        let mut found: Vec<(HistoryEntry, Matched)> = Vec::new();
        for u in &urls {
            for h in db.history_of_url(u).await? {
                if !found.iter().any(|(f, _)| f.id == h.id) {
                    found.push((h, Matched::Url));
                }
            }
        }
        if found.is_empty()
            && let (Some(name), Some(size)) = (name, size)
        {
            let wanted = plain_name(name);
            found = db
                .history_of_size(size)
                .await?
                .into_iter()
                .filter(|h| plain_name(&h.name) == wanted)
                .map(|h| (h, Matched::NameSize))
                .collect();
        }
        let best = match found.iter().position(|(h, _)| on_disk(h)) {
            Some(i) => Some(found.swap_remove(i)),
            None => found.into_iter().next(),
        };
        Ok(match best {
            Some((h, matched)) => Some(Previous::of(h, matched, db).await),
            None => None,
        })
    }

    /// A group for downloads about to be added together; give its id as
    /// [`NewTask::group`].
    pub async fn new_group(&self, name: &str) -> Result<Group> {
        let name = name.trim();
        let name = if name.is_empty() { "Downloads" } else { name };
        let added_at = now_ms();
        let id = self.inner.db.new_group(name, added_at).await?;
        Ok(Group {
            id,
            name: name.to_owned(),
            added_at,
        })
    }

    /// Groups that still have downloads on the list, newest first.
    pub async fn groups(&self) -> Result<Vec<Group>> {
        self.inner.db.groups().await
    }

    /// Where the file at `path` came from: the download that saved it (by its path, or by
    /// its size and SHA-256), or the mark the browser left on it.
    pub async fn file_origin(&self, path: &Path) -> Result<origin::Origin> {
        origin::trace(&self.inner.db, path).await
    }

    /// The download of a file known only by its `size` and its SHA-256 (hex) or `name`.
    pub async fn find_by_content(
        &self,
        size: u64,
        sha256: Option<&str>,
        name: Option<&str>,
    ) -> Result<Option<HistoryEntry>> {
        origin::trace_content(&self.inner.db, size, sha256, name).await
    }

    /// The file at `path` hashed again, against the checksum `recorded` with its download,
    /// or without one against what the publisher of `url` posts now.
    pub async fn check_file_checksum(
        &self,
        path: &Path,
        recorded: Option<&str>,
        url: Option<&str>,
    ) -> Result<origin::ChecksumCheck> {
        let opts = lock(&self.inner.config).engine.clone();
        origin::check_checksum(path, recorded, url, &opts).await
    }

    /// Asks `url` whether it serves something else now than the `size` bytes downloaded at
    /// `downloaded_at`, and for a GitHub release asset which release is the newest.
    pub async fn check_for_update(
        &self,
        url: &str,
        size: u64,
        downloaded_at: Option<i64>,
    ) -> Result<origin::UpdateCheck> {
        let opts = lock(&self.inner.config).engine.clone();
        origin::check_update(url, size, downloaded_at, &opts).await
    }

    /// Finished downloads, newest first, also those no longer on the list.
    pub async fn history(&self) -> Result<Vec<HistoryEntry>> {
        self.inner.db.history().await
    }

    pub async fn history_entry(&self, id: i64) -> Result<Option<HistoryEntry>> {
        self.inner.db.history_entry(id).await
    }

    /// Forgets every finished download (the list keeps its own; their files stay).
    pub async fn clear_history(&self) -> Result<()> {
        self.inner.db.clear_history().await
    }
}
