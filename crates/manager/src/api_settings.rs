//! Speed limits, the site grabber, archives and torrents to inspect, fresh links,
//! logins and headers, the sites' memory, closing.

use super::*;

impl Manager {
    /// Limit for all downloads together, bytes per second; takes effect at once.
    pub fn set_global_limit(&self, rate: Option<u64>) {
        lock(&self.inner.net).user_limit = rate;
        self.inner.apply_limit();
    }

    /// The network and the power as last seen, and this month's data.
    pub fn net_status(&self) -> net::NetStatus {
        let rules = lock(&self.inner.config).net_rules.clone();
        let net = lock(&self.inner.net);
        let used = net.usage.now(now_ms());
        net::NetStatus {
            conditions: net.conditions.clone(),
            usage: net::Usage {
                month: net::month_of(now_ms()),
                bytes: used,
            },
            budget: rules.monthly_budget,
            over_budget: rules.budget_stop && rules.monthly_budget.is_some_and(|b| used >= b),
        }
    }

    /// Reads `url` and the pages it links to, down to `opts.depth`, and returns the files
    /// they link to (see [`grabber`]), with the client settings downloads use.
    pub async fn grab_site(
        &self,
        url: &str,
        opts: &grabber::GrabOptions,
        progress: impl Fn(grabber::GrabProgress),
        cancel: &CancellationToken,
    ) -> Result<Vec<grabber::Found>> {
        let engine = lock(&self.inner.config).engine.clone();
        // A password-protected folder opens with the login saved for its site.
        let headers: Vec<(String, String)> = self
            .saved_login(url)
            .await?
            .map(|(user, password)| shardload_engine::basic_auth(&user, &password))
            .into_iter()
            .collect();
        grabber::grab(url, opts, &headers, &engine, progress, cancel)
            .await
            .map_err(|e| shardload_engine::Error::State(e).into())
    }

    /// The name and files of a torrent (a magnet link, or a `.torrent` file on the web or on
    /// disk), without adding it: for choosing files before the download starts.
    pub async fn inspect_torrent(&self, url: &str) -> Result<TorrentInfo> {
        self.inner
            .torrents
            .inspect(url)
            .await
            .map_err(|e| shardload_engine::Error::State(e).into())
    }

    /// The files in a ZIP archive on the server, read without downloading it (the server must
    /// support range requests; [`shardload_engine::Error::NoRanges`] otherwise): for choosing
    /// which to download.
    pub async fn inspect_archive(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<ArchiveInfo> {
        let opts = lock(&self.inner.config).engine.clone();
        let zip = shardload_engine::RemoteZip::open(url, headers, &opts).await?;
        Ok(ArchiveInfo {
            name: zip.name().to_owned(),
            size: zip.size(),
            zip64: zip.is_zip64(),
            entries: zip
                .entries()
                .iter()
                .map(|e| ArchiveEntry {
                    name: e.name.clone(),
                    size: e.size,
                    compressed_size: e.compressed_size,
                    method: shardload_engine::method_name(e.method),
                    is_dir: e.is_dir,
                    encrypted: e.encrypted,
                    unsupported: e.unsupported(),
                })
                .collect(),
        })
    }

    pub fn global_limit(&self) -> Option<u64> {
        lock(&self.inner.net).user_limit
    }

    /// One download's own limit, bytes per second; takes effect at once if it is running.
    pub async fn set_speed_limit(&self, id: TaskId, rate: Option<u64>) -> Result<()> {
        self.inner.db.set_speed_limit(id, rate).await?;
        if let Some(r) = lock(&self.inner.running).get(&id) {
            r.limiter.set_rate(rate);
        }
        let task = self.get(id).await?;
        self.inner.emit(Event::Updated(task));
        Ok(())
    }

    /// Points a download at a fresh link for the same file (the old one expired, or the site
    /// hands out a new one per visit), keeping its progress. The new link must serve the same
    /// file: the same size, and the same bytes as already downloaded; otherwise this fails
    /// with [`shardload_engine::Error::FileChanged`] and nothing changes. A download that was
    /// running or had failed continues with the new link.
    ///
    /// `headers` from the browser (fresh cookies, referrer) replace the download's headers of
    /// the same names; others, such as a login, are kept.
    pub async fn refresh_link(
        &self,
        id: TaskId,
        url: &str,
        headers: Option<&[(String, String)]>,
    ) -> Result<()> {
        let task = self.get(id).await?;
        let headers = match headers {
            Some(fresh) => merge_headers(&task.headers, fresh),
            None => task.headers.clone(),
        };
        let was_running = self.inner.stop(id, Stop::Pause).await;
        if let Some(path) = &task.path {
            let mut state = path.as_os_str().to_owned();
            state.push(".shardload-state");
            if PathBuf::from(state).exists() {
                let opts = lock(&self.inner.config).engine.clone();
                let req = shardload_engine::Request {
                    url: url.to_owned(),
                    output: path.clone(),
                    headers: headers.clone(),
                    mirrors: Vec::new(),
                };
                if let Err(e) = shardload_engine::patch_url(&req, &opts).await {
                    if was_running {
                        self.start(id).await?;
                    }
                    return Err(e.into());
                }
            }
        }
        self.inner.db.set_url(id, url, now_ms()).await?;
        self.inner.db.set_headers(id, &headers).await?;
        self.inner.emit(Event::Updated(self.get(id).await?));
        if was_running || task.status == Status::Error {
            self.start(id).await?;
        }
        Ok(())
    }

    /// Sends these credentials (HTTP Basic) with every request of the download from now on,
    /// replacing earlier ones, and queues it: for a server that asked for a login.
    pub async fn set_login(&self, id: TaskId, user: &str, password: &str) -> Result<()> {
        let task = self.get(id).await?;
        let was_running = self.inner.stop(id, Stop::Pause).await;
        let mut headers: Vec<(String, String)> = task
            .headers
            .into_iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("authorization"))
            .collect();
        headers.push(shardload_engine::basic_auth(user, password));
        self.inner.db.set_headers(id, &headers).await?;
        if was_running || task.status != Status::Done {
            self.inner.set_status(id, Status::Queued, None).await?;
        }
        self.inner.fill().await
    }

    /// Sends these request headers with the download from now on, in place of the ones it
    /// had (cookies, referrer, a login…). A running download continues with them where it
    /// was; a failed one is tried again.
    pub async fn set_headers(&self, id: TaskId, headers: &[(String, String)]) -> Result<Task> {
        command::check_headers(headers).map_err(Error::BadHeaders)?;
        let task = self.get(id).await?;
        let was_running = self.inner.stop(id, Stop::Pause).await;
        self.inner.db.set_headers(id, headers).await?;
        if was_running || task.status == Status::Error {
            lock(&self.inner.retries).remove(&id);
            self.inner.set_status(id, Status::Queued, None).await?;
        }
        let task = self.get(id).await?;
        self.inner.emit(Event::Updated(task.clone()));
        self.inner.fill().await?;
        Ok(task)
    }

    /// Remembers a login for `site` (see [`site_of`]); a download there that the server asks
    /// to log in to uses it without asking.
    pub async fn save_login(&self, site: &str, user: &str, password: &str) -> Result<()> {
        self.inner.db.save_login(site, user, password).await
    }

    /// The login saved for the site of `url`.
    pub async fn saved_login(&self, url: &str) -> Result<Option<(String, String)>> {
        match site_of(url) {
            Some(site) => self.inner.db.login(&site).await,
            None => Ok(None),
        }
    }

    pub async fn saved_logins(&self) -> Result<Vec<SavedLogin>> {
        Ok(self
            .inner
            .db
            .logins()
            .await?
            .into_iter()
            .map(|(site, user)| SavedLogin { site, user })
            .collect())
    }

    pub async fn forget_login(&self, site: &str) -> Result<()> {
        self.inner.db.forget_login(site).await
    }

    /// What downloads learned about each site (see [`sites`]), most recently seen first;
    /// memories older than [`sites::FORGET_AFTER`] are left out (and deleted).
    pub async fn sites(&self) -> Result<Vec<SiteMemory>> {
        let now = now_ms();
        let mut kept = Vec::new();
        for m in self.inner.db.sites().await? {
            if let Some(m) = sites::recall(&self.inner.db, &m.site, now).await? {
                kept.push(m);
            }
        }
        Ok(kept)
    }

    /// What is remembered about the site of `url`.
    pub async fn site_memory(&self, url: &str) -> Result<Option<SiteMemory>> {
        match site_of(url) {
            Some(site) => sites::recall(&self.inner.db, &site, now_ms()).await,
            None => Ok(None),
        }
    }

    /// Waiting downloads (queued or paused) whose link, by how long links of that site lasted
    /// before, is about to stop working or already has: start them sooner, or get a fresh
    /// link from the browser. A guess; the download is not stopped from trying.
    pub async fn link_warnings(&self) -> Result<Vec<LinkWarning>> {
        let now = now_ms();
        let mut lifetimes: HashMap<String, Option<u64>> = HashMap::new();
        let mut out = Vec::new();
        for task in self.inner.db.list().await? {
            if !matches!(task.status, Status::Queued | Status::Paused) {
                continue;
            }
            let Some(site) = site_of(&task.url) else {
                continue;
            };
            let lifetime = match lifetimes.get(&site) {
                Some(l) => *l,
                None => {
                    let l = sites::recall(&self.inner.db, &site, now)
                        .await?
                        .and_then(|m| m.link_lifetime);
                    lifetimes.insert(site, l);
                    l
                }
            };
            if let Some(w) = lifetime.and_then(|l| LinkWarning::of(task.id, task.link_at, l, now)) {
                out.push(w);
            }
        }
        Ok(out)
    }

    /// Forgets what downloads learned about `site`; the next one finds out again.
    pub async fn forget_site(&self, site: &str) -> Result<()> {
        self.inner.db.forget_site(site).await
    }

    /// Stops every running download so it resumes on the next [`Manager::open`], and closes
    /// the database.
    pub async fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        // A fill already starting downloads has them running once it is over.
        drop(self.inner.scheduling.lock().await);
        let ids: Vec<TaskId> = lock(&self.inner.running).keys().copied().collect();
        for id in ids {
            self.inner.stop(id, Stop::Shutdown).await;
        }
        self.inner.torrents.close().await;
        self.inner.db.close().await;
    }
}
