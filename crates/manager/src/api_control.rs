//! What is done to downloads: start, pause, remove, download again, the queue's order,
//! renaming and moving.

use super::*;

impl Manager {
    /// Queues a paused, failed or queued download; it starts when a slot is free.
    pub async fn start(&self, id: TaskId) -> Result<()> {
        let task = self.get(id).await?;
        // Started by hand: no waiting for a scheduled retry, or for its set time.
        lock(&self.inner.retries).remove(&id);
        lock(&self.inner.later).remove(&id);
        if matches!(task.status, Status::Paused | Status::Error) {
            self.inner.set_status(id, Status::Queued, None).await?;
        }
        self.inner.fill().await
    }

    /// Stops a running download (keeping its progress) or takes a queued one out of the
    /// queue. Returns once the download has stopped.
    pub async fn pause(&self, id: TaskId) -> Result<()> {
        let task = self.get(id).await?;
        lock(&self.inner.retries).remove(&id);
        if !self.inner.stop(id, Stop::Pause).await && task.status == Status::Queued {
            self.inner.set_status(id, Status::Paused, None).await?;
        }
        Ok(())
    }

    /// Removes a download from the list, stopping it first. With `delete_files`, the file
    /// and any partial data go too; otherwise files stay where they are.
    pub async fn remove(&self, id: TaskId, delete_files: bool) -> Result<()> {
        let task = self.get(id).await?;
        lock(&self.inner.retries).remove(&id);
        self.inner.stop(id, Stop::Remove).await;
        self.inner.db.delete(id).await?;
        if delete_files && let Some(path) = &task.path {
            delete_files_of(path);
            // A torrent with several files, or files out of an archive: the folder chosen
            // for them on the first start.
            let folder = torrent::is_torrent(&task.url)
                || task.stream.as_ref().is_some_and(|s| s.archive.is_some());
            if folder && path.is_dir() {
                std::fs::remove_dir_all(path).ok();
            }
        }
        self.inner.emit(Event::Removed(id));
        self.inner.fill().await
    }

    /// Downloads again from scratch (IDM's "Redownload"): the file and any partial data are
    /// deleted and the download is queued.
    pub async fn restart(&self, id: TaskId) -> Result<()> {
        let task = self.get(id).await?;
        lock(&self.inner.retries).remove(&id);
        // Stopped like a removal: the download's own end is not recorded.
        self.inner.stop(id, Stop::Remove).await;
        if let Some(path) = &task.path {
            delete_files_of(path);
            // Files out of an archive: their folder, or they would count as done.
            if task.stream.as_ref().is_some_and(|s| s.archive.is_some()) && path.is_dir() {
                std::fs::remove_dir_all(path).ok();
            }
        }
        self.inner.db.reset(id).await?;
        let task = self.get(id).await?;
        self.inner.emit(Event::Updated(task));
        self.inner.fill().await
    }

    /// Asks the server about `url` with the configured client settings, without adding
    /// anything: for showing name and size before the user decides.
    pub async fn probe(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<shardload_engine::Probe> {
        let opts = lock(&self.inner.config).engine.clone();
        Ok(shardload_engine::probe(url, headers, &opts).await?)
    }

    /// Queues every paused or failed download.
    pub async fn start_all(&self) -> Result<()> {
        for t in self.list().await? {
            if matches!(t.status, Status::Paused | Status::Error) {
                lock(&self.inner.retries).remove(&t.id);
                self.inner.set_status(t.id, Status::Queued, None).await?;
            }
        }
        self.inner.fill().await
    }

    /// Pauses every running and queued download.
    pub async fn pause_all(&self) -> Result<()> {
        // Queued ones first, so stopping a running one does not start the next.
        let tasks = self.list().await?;
        for status in [Status::Queued, Status::Downloading] {
            for t in tasks.iter().filter(|t| t.status == status) {
                self.pause(t.id).await?;
            }
        }
        Ok(())
    }

    /// Moves a download in the queue order, among the downloads that are not finished.
    pub async fn move_task(&self, id: TaskId, to: Move) -> Result<()> {
        let tasks = self.list().await?; // in queue order
        let order: Vec<&Task> = tasks.iter().filter(|t| t.status != Status::Done).collect();
        let Some(i) = order.iter().position(|t| t.id == id) else {
            return Ok(());
        };
        let mut changed = vec![];
        match to {
            Move::Top => {
                let first = tasks.first().map_or(0, |t| t.queue_pos);
                self.inner.db.set_queue_pos(id, first - 1).await?;
                changed.push(id);
            }
            Move::Bottom => {
                let last = tasks.last().map_or(0, |t| t.queue_pos);
                self.inner.db.set_queue_pos(id, last + 1).await?;
                changed.push(id);
            }
            Move::Up | Move::Down => {
                let j = if to == Move::Up {
                    i.checked_sub(1)
                } else {
                    Some(i + 1).filter(|j| *j < order.len())
                };
                let Some(j) = j else { return Ok(()) };
                let (a, b) = (order[i], order[j]);
                self.inner.db.set_queue_pos(a.id, b.queue_pos).await?;
                self.inner.db.set_queue_pos(b.id, a.queue_pos).await?;
                changed.extend([a.id, b.id]);
            }
        }
        for id in changed {
            self.inner.emit(Event::Updated(self.get(id).await?));
        }
        Ok(())
    }

    /// Gives a download another file name, cleaned like a name from a server; see
    /// [`Manager::relocate`] for how its files follow. A torrent's or an archive's folder is
    /// renamed; their names are known only once they start, so before that this fails.
    pub async fn rename(&self, id: TaskId, name: &str) -> Result<Task> {
        let name = shardload_engine::name::sanitize(name);
        self.place(id, None, Some(name)).await
    }

    /// Moves a download into another folder.
    ///
    /// A download with nothing on disk yet only changes in the list. Otherwise its files go
    /// along: the finished file, or the partial data and state of an unfinished one (a
    /// running download is paused for it and continues where it was), with a stream's
    /// subtitles and a torrent's or an archive's whole folder. On the same drive this is a
    /// rename; onto another drive the files are copied, checked and only then deleted, with
    /// [`Event::Moving`] as it goes. A name taken in the new place gets a number,
    /// `name (1).ext`.
    pub async fn relocate(&self, id: TaskId, dir: &Path) -> Result<Task> {
        self.place(id, Some(dir.to_owned()), None).await
    }

    /// Whether some download's files are being renamed or moved right now.
    pub fn is_moving(&self) -> bool {
        !lock(&self.inner.moving).is_empty()
    }

    pub(crate) async fn place(
        &self,
        id: TaskId,
        dir: Option<PathBuf>,
        name: Option<String>,
    ) -> Result<Task> {
        self.get(id).await?;
        if !lock(&self.inner.moving).insert(id) {
            return Err(Error::CannotMove(
                "its files are being moved already".into(),
            ));
        }
        // A queue fill that read the download as queued just before may be starting it;
        // once that is over, nothing starts it until the move is done.
        drop(self.inner.scheduling.lock().await);
        let was_running = self.inner.stop(id, Stop::Pause).await;
        let result = self.place_stopped(id, dir, name).await;
        lock(&self.inner.moving).remove(&id);
        if was_running {
            self.inner.db.set_status(id, Status::Queued, None).await?;
        }
        // Also after a failure: the list stops showing the move.
        let task = self.get(id).await?;
        self.inner.emit(Event::Updated(task.clone()));
        self.inner.fill().await?;
        result.map(|()| task)
    }

    pub(crate) async fn place_stopped(
        &self,
        id: TaskId,
        dir: Option<PathBuf>,
        name: Option<String>,
    ) -> Result<()> {
        let task = self.get(id).await?;
        let dir = dir.unwrap_or_else(|| task.dir.clone());
        let mut stream = task.stream.clone();
        let is_torrent = torrent::is_torrent(&task.url);
        let torrent_folder = stream
            .as_ref()
            .and_then(|s| s.torrent.as_ref())
            .and_then(|c| c.folder.clone());
        let archive = stream.as_ref().and_then(|s| s.archive.as_ref());
        let archive_folder = archive.and_then(|c| c.folder.clone());
        let named_on_start = is_torrent || archive.is_some_and(|a| !a.whole);

        let Some(path) = task.path.clone() else {
            // Not started: the name and folder are used when it starts.
            if name.is_some() && named_on_start {
                return Err(Error::CannotMove(
                    "its name is known once it starts; rename it then".into(),
                ));
            }
            let category = match &name {
                Some(n) => Category::for_name(n),
                None => task.category,
            };
            let name = name.or(task.name);
            return self
                .inner
                .db
                .set_place(id, &dir, name.as_deref(), None, category)
                .await;
        };

        // What moves: a torrent's or an archive's own folder, or the file.
        let own_folder = torrent_folder.or(archive_folder).filter(|f| *f != task.dir);
        let unit = own_folder.clone().unwrap_or_else(|| path.clone());
        let unit_name = name.clone().unwrap_or_else(|| file_name(&unit));
        // An unfinished torrent's file straight in the folder must keep the torrent's name.
        let fixed_name = is_torrent && own_folder.is_none() && task.status != Status::Done;
        if fixed_name && name.is_some() {
            return Err(Error::CannotMove(
                "a torrent's file can be renamed once it is finished".into(),
            ));
        }

        let _one_at_a_time = self.inner.naming.lock().await;
        let wanted = dir.join(&unit_name);
        let new_unit = if relocate::same_path(&wanted, &unit) {
            wanted.clone()
        } else {
            let taken: HashSet<PathBuf> =
                self.inner.db.paths_except(id).await?.into_iter().collect();
            shardload_engine::target_avoiding(&dir, &unit_name, "", Conflict::Rename, |p| {
                taken.contains(p)
            })
        };
        if new_unit == unit {
            return Ok(());
        }
        if fixed_name && new_unit != wanted {
            return Err(Error::CannotMove(format!(
                "{unit_name} is taken in that folder"
            )));
        }

        let members = if own_folder.is_some() {
            Some(unit.clone())
                .filter(|p| p.exists())
                .into_iter()
                .collect()
        } else {
            let stream = task
                .stream
                .as_ref()
                .is_some_and(|s| s.torrent.is_none() && s.archive.is_none());
            relocate::members(&unit, stream)
        };
        if members.is_empty() && task.status == Status::Done {
            return Err(Error::CannotMove(format!(
                "{} is no longer there",
                unit.display()
            )));
        }
        let moves: Vec<(PathBuf, PathBuf)> = members
            .into_iter()
            .map(|m| {
                let to = relocate::destination(&m, &unit, &new_unit);
                (m, to)
            })
            .collect();
        let events = self.inner.events.clone();
        tokio::task::spawn_blocking(move || {
            relocate::move_all(&moves, false, |copied, total| {
                let _ = events.send(Event::Moving(MoveProgress { id, copied, total }));
            })
        })
        .await
        .map_err(|e| shardload_engine::Error::from(std::io::Error::other(e)))?
        .map_err(shardload_engine::Error::from)?;

        let new_path = match path.strip_prefix(&unit) {
            Ok(rel) if !rel.as_os_str().is_empty() => new_unit.join(rel),
            _ => new_unit.clone(),
        };
        if let Some(s) = stream.as_mut() {
            if let Some(c) = s.torrent.as_mut() {
                c.folder = Some(match own_folder {
                    Some(_) => new_unit.clone(),
                    None => dir.clone(),
                });
            }
            if let Some(c) = s.archive.as_mut().filter(|c| c.folder.is_some()) {
                c.folder = Some(new_unit.clone());
            }
            self.inner.db.set_stream(id, s).await?;
        }
        let new_name = file_name(&new_unit);
        let category = if own_folder.is_none() && !is_torrent && name.is_some() {
            Category::for_name(&new_name)
        } else {
            task.category
        };
        self.inner
            .db
            .set_place(id, &dir, Some(&new_name), Some(&new_path), category)
            .await
    }
}
