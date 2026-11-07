//! Downloading torrents and pages of video sites (through yt-dlp).

use super::*;

impl Inner {
    /// A torrent: on the first start its files are listed (for a magnet link, asked from
    /// peers) to pick where they go: a single file straight into the download folder, several
    /// into a folder named after the torrent. The choice is kept for later starts.
    pub(crate) async fn download_torrent(
        &self,
        mut task: Task,
        conflict: Conflict,
        limit: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<Outcome> {
        let started = std::time::Instant::now();
        let state = |e: String| -> Error { shardload_engine::Error::State(e).into() };
        let mut stream = task.stream.clone().unwrap_or_default();
        let mut choice = stream.torrent.clone().unwrap_or_default();
        let folder = match choice.folder.clone() {
            Some(f) => f,
            None => {
                let info = tokio::select! {
                    i = self.torrents.inspect(&task.url) => i.map_err(state)?,
                    () = cancel.cancelled() => return Err(shardload_engine::Error::Cancelled.into()),
                };
                let chosen: Vec<&TorrentFile> = match &choice.files {
                    Some(ids) => ids.iter().filter_map(|&i| info.files.get(i)).collect(),
                    None => info.files.iter().collect(),
                };
                let size: u64 = chosen.iter().map(|f| f.size).sum();
                std::fs::create_dir_all(&task.dir).map_err(shardload_engine::Error::from)?;
                let _one_at_a_time = self.naming.lock().await;
                let taken: std::collections::HashSet<PathBuf> =
                    self.db.paths_except(task.id).await?.into_iter().collect();
                let free = |p: &Path| !p.exists() && !taken.contains(p);
                let base = shardload_engine::name::sanitize(&info.name);
                let (folder, path, name) = match info.files.as_slice() {
                    // One file: straight into the folder unless the name is taken there.
                    [only] if free(&task.dir.join(&only.name)) => (
                        task.dir.clone(),
                        task.dir.join(&only.name),
                        only.name.clone(),
                    ),
                    _ => {
                        let folder = shardload_engine::target_avoiding(
                            &task.dir,
                            &base,
                            &task.url,
                            conflict,
                            |p| taken.contains(p),
                        );
                        let name = folder
                            .file_name()
                            .map_or_else(|| base.clone(), |n| n.to_string_lossy().into_owned());
                        let path = match info.files.as_slice() {
                            [only] => folder.join(&only.name),
                            _ => folder.clone(),
                        };
                        (folder, path, name)
                    }
                };
                let category = match chosen.as_slice() {
                    [one] => Category::for_name(&one.name),
                    _ => Category::for_name(&name),
                };
                self.db
                    .set_target(task.id, &name, &path, Some(size), category)
                    .await?;
                choice.folder = Some(folder.clone());
                stream.torrent = Some(choice.clone());
                self.db.set_stream(task.id, &stream).await?;
                task.name = Some(name);
                task.path = Some(path);
                task.size = Some(size);
                task.category = category;
                task.stream = Some(stream);
                self.emit(Event::Updated(task.clone()));
                folder
            }
        };
        self.torrents.set_limit(self.global.rate());
        let (tx, rx) = watch::channel(Progress::default());
        let reporter = tokio::spawn(report(
            task.id,
            rx,
            self.events.clone(),
            self.db.clone(),
            None,
            hints::Watch::default(),
            self.speeds.clone(),
        ));
        let result = self
            .torrents
            .download(&task.url, &folder, choice.files, limit, &tx, cancel)
            .await;
        drop(tx);
        reporter.await.ok();
        match result {
            Ok(done) => Ok(Outcome {
                size: done.size,
                resumed_from: 0,
                elapsed: started.elapsed(),
                checksum: None,
                checksum_from: None,
                structure: None,
            }),
            Err(_) if cancel.is_cancelled() => Err(shardload_engine::Error::Cancelled.into()),
            Err(e) => Err(state(e)),
        }
    }

    /// A page of a site yt-dlp knows: the engine downloads the chosen formats from the
    /// addresses yt-dlp gives (see [`ytdlp::download_direct`]), or yt-dlp itself when that
    /// cannot be done (a clip, a recording, a stream in fragments); the file's real
    /// extension is known only at the end. `opts` are the engine's, with the speed limits;
    /// their `mark_of_the_web` gives the file the Mark of the Web, with the page as its
    /// address.
    pub(crate) async fn download_page(
        &self,
        mut task: Task,
        format: &str,
        tools: &ytdlp::Tools,
        opts: &Options,
        conflict: Conflict,
        cancel: &CancellationToken,
    ) -> Result<Outcome> {
        let mark = opts.mark_of_the_web;
        let started = std::time::Instant::now();
        let path = match &task.path {
            Some(p) => p.clone(),
            None => {
                std::fs::create_dir_all(&task.dir).map_err(shardload_engine::Error::from)?;
                let stem = task.name.clone().unwrap_or_else(|| "video".to_owned());
                let stem = match &task.stream {
                    Some(s) => clip_stem(shardload_engine::name::split_extension(&stem).0, s),
                    None => shardload_engine::name::split_extension(&stem).0.to_owned(),
                };
                let name = shardload_engine::name::sanitize(&format!("{stem}.mp4"));
                let _one_at_a_time = self.naming.lock().await;
                let taken: std::collections::HashSet<PathBuf> =
                    self.db.paths_except(task.id).await?.into_iter().collect();
                let path =
                    shardload_engine::target_avoiding(&task.dir, &name, &task.url, conflict, |p| {
                        taken.contains(p)
                    });
                let file_name = path
                    .file_name()
                    .map_or_else(|| name.clone(), |n| n.to_string_lossy().into_owned());
                self.db
                    .set_target(task.id, &file_name, &path, None, Category::Video)
                    .await?;
                task.name = Some(file_name);
                task.path = Some(path.clone());
                task.category = Category::Video;
                self.emit(Event::Updated(task.clone()));
                path
            }
        };
        let stem = path.with_extension("");
        let (tx, rx) = watch::channel(Progress::default());
        let reporter = tokio::spawn(report(
            task.id,
            rx,
            self.events.clone(),
            self.db.clone(),
            None,
            hints::Watch::default(),
            self.speeds.clone(),
        ));
        let clip = task.stream.as_ref().and_then(|s| s.clip);
        let record = task.stream.as_ref().and_then(|s| s.record.clone());
        // A recording ends at its limit like the user stopping it.
        let stop = cancel.child_token();
        if let Some(limit) = record.as_ref().and_then(|r| r.limit) {
            let stop = stop.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(limit)).await;
                stop.cancel();
            });
        }
        let result = fetch_page(
            &task,
            format,
            tools,
            opts,
            clip.as_ref(),
            record.as_ref(),
            &stem,
            &tx,
            &stop,
        )
        .await;
        stop.cancel();
        drop(tx);
        reporter.await.ok();
        let mut file = match result {
            Ok(f) => f,
            Err(_) if cancel.is_cancelled() => {
                return Err(shardload_engine::Error::Cancelled.into());
            }
            Err(e) => return Err(shardload_engine::Error::State(e).into()),
        };
        // A recording is written as MPEG-TS, whatever its name: an MP4 with ffmpeg.
        if record.is_some()
            && let Some(ffmpeg) = &tools.ffmpeg
        {
            match ytdlp::remux(ffmpeg, &file, &file.with_extension("mp4")).await {
                Ok(mp4) => file = mp4,
                Err(e) => tracing::warn!(id = task.id, "recording kept as MPEG-TS: {e}"),
            }
        }
        if file != path {
            // Not merged into MP4 (audio alone, or a single file of another type).
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let category = Category::for_name(&name);
            self.db
                .set_target(task.id, &name, &file, None, category)
                .await?;
        }
        let size = std::fs::metadata(&file)
            .map_err(shardload_engine::Error::from)?
            .len();
        if mark {
            shardload_engine::mark_of_the_web(&file, &task.url, &task.headers);
        }
        Ok(Outcome {
            size,
            resumed_from: 0,
            elapsed: started.elapsed(),
            checksum: None,
            checksum_from: None,
            structure: None,
        })
    }
}

/// The page's file, written to `<stem>.<ext>` by the engine from the addresses yt-dlp gives,
/// or by yt-dlp itself when that cannot be done (a clip, a recording, a stream in
/// fragments, a link that stops working).
#[allow(clippy::too_many_arguments, reason = "a download's parts, passed down")]
async fn fetch_page(
    task: &Task,
    format: &str,
    tools: &ytdlp::Tools,
    opts: &Options,
    clip: Option<&crate::clip::Clip>,
    record: Option<&crate::task::Recording>,
    stem: &Path,
    tx: &watch::Sender<Progress>,
    stop: &CancellationToken,
) -> std::result::Result<PathBuf, String> {
    let direct = if clip.is_none() && record.is_none() {
        ytdlp::download_direct(
            tools,
            &task.url,
            format,
            stem,
            &task.headers,
            opts,
            tx,
            stop,
        )
        .await
    } else {
        ytdlp::Direct::Fallback("a clip or a recording".into())
    };
    match direct {
        ytdlp::Direct::Done(file) => Ok(file),
        ytdlp::Direct::Cancelled => Err("cancelled".to_owned()),
        ytdlp::Direct::Fallback(why) => {
            tracing::info!(id = task.id, "yt-dlp downloads the page itself: {why}");
            tx.send_replace(Progress::default());
            let file = ytdlp::run(
                tools,
                &task.url,
                format,
                clip,
                record,
                false,
                stem,
                &task.headers,
                task.connections,
                tx,
                stop,
            )
            .await;
            let Some(r) = record else { return file };
            // A recording that failed along the way keeps what it wrote, and goes on.
            let file = match file {
                Ok(f) => f,
                Err(e) => match ytdlp::written_file(stem)
                    .filter(|f| std::fs::metadata(f).is_ok_and(|m| m.len() > 0))
                {
                    Some(f) => {
                        tracing::warn!(id = task.id, "the recording broke off: {e}");
                        f
                    }
                    None => return Err(e),
                },
            };
            Ok(continue_recording(task, format, tools, r, file, stem, tx, stop).await)
        }
    }
}

/// Parts recorded after the first, at most: a broadcast that keeps dropping is let go.
const MAX_PARTS: u32 = 50;

/// A recording whose ffmpeg stopped on its own while the user had not stopped it (a hiccup
/// reading the live playlist ends it like the broadcast ending): recorded on into further
/// parts while the broadcast is live, each added to the end of `file` (MPEG-TS joins by
/// putting one after the other). Returns `file`, with what the parts added.
#[allow(clippy::too_many_arguments, reason = "a download's parts, passed down")]
async fn continue_recording(
    task: &Task,
    format: &str,
    tools: &ytdlp::Tools,
    record: &crate::task::Recording,
    file: PathBuf,
    stem: &Path,
    tx: &watch::Sender<Progress>,
    stop: &CancellationToken,
) -> PathBuf {
    for part in 1..=MAX_PARTS {
        if stop.is_cancelled() {
            break;
        }
        let before = tx.borrow().clone();
        let offset = std::fs::metadata(&file).map_or(0, |m| m.len());
        tracing::info!(
            id = task.id,
            part,
            "the recording stopped on its own: recording on"
        );
        let name = stem
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let part_stem = stem.with_file_name(format!("{name}.part{part}"));
        // The part's progress, shown after what was recorded before it.
        let (ptx, mut prx) = watch::channel(Progress::default());
        let forward = {
            let tx = tx.clone();
            async move {
                while prx.changed().await.is_ok() {
                    let p = prx.borrow().clone();
                    if p.received == 0 {
                        continue;
                    }
                    tx.send_modify(|q| {
                        *q = Progress {
                            received: offset + p.received,
                            recorded: before.recorded + p.recorded,
                            ..p
                        };
                    });
                }
            }
        };
        let (got, ()) = tokio::join!(
            async {
                let got = ytdlp::run(
                    tools,
                    &task.url,
                    format,
                    None,
                    Some(record),
                    true,
                    &part_stem,
                    &task.headers,
                    task.connections,
                    &ptx,
                    stop,
                )
                .await;
                drop(ptx);
                got
            },
            forward
        );
        let Ok(piece) = got else { break };
        let added = append(&file, &piece);
        std::fs::remove_file(&piece).ok();
        if !added {
            break;
        }
    }
    file
}

/// Adds `piece` to the end of `file`; whether anything was added.
fn append(file: &Path, piece: &Path) -> bool {
    let Ok(mut from) = std::fs::File::open(piece) else {
        return false;
    };
    let Ok(mut to) = std::fs::OpenOptions::new().append(true).open(file) else {
        return false;
    };
    std::io::copy(&mut from, &mut to).is_ok_and(|n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_is_added_to_the_end_of_the_recording() {
        let dir = std::env::temp_dir().join(format!("shardload-append-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let (file, piece, empty) = (dir.join("a.ts"), dir.join("b.ts"), dir.join("c.ts"));
        std::fs::write(&file, b"first").expect("write");
        std::fs::write(&piece, b"second").expect("write");
        std::fs::write(&empty, b"").expect("write");
        assert!(append(&file, &piece));
        assert!(!append(&file, &empty));
        assert!(!append(&file, &dir.join("missing.ts")));
        assert_eq!(std::fs::read(&file).expect("read"), b"firstsecond");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A live broadcast (`SHARDLOAD_LIVE`, a page yt-dlp knows) recorded for 70 seconds, its
    /// ffmpeg killed twenty seconds in, as a hiccup would end it: the recording goes on.
    #[tokio::test]
    #[ignore = "network: a live broadcast, and SHARDLOAD_TOOLS"]
    async fn a_recording_whose_ffmpeg_stops_goes_on() {
        let dir = PathBuf::from(std::env::var("SHARDLOAD_TOOLS").expect("SHARDLOAD_TOOLS"));
        let tools = ytdlp::Tools {
            ytdlp: dir.join("yt-dlp.exe"),
            deno: Some(dir.join("deno.exe")),
            ffmpeg: None,
        };
        let task = crate::task::tests::sample(
            &std::env::var("SHARDLOAD_LIVE").expect("SHARDLOAD_LIVE"),
            "live",
        );
        let out = std::env::temp_dir().join(format!("shardload-live-{}", std::process::id()));
        std::fs::create_dir_all(&out).expect("dir");
        let stem = out.join("live");
        let record = crate::task::Recording::default();
        let (tx, _rx) = watch::channel(Progress::default());
        let stop = CancellationToken::new();
        let limit = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(20)).await;
            std::process::Command::new("taskkill")
                .args(["/F", "/IM", "ffmpeg.exe"])
                .output()
                .ok();
            tokio::time::sleep(Duration::from_secs(50)).await;
            limit.cancel();
        });
        let file = fetch_page(
            &task,
            "bestvideo[height<=480]+bestaudio/best[height<=480]/best",
            &tools,
            &Options::default(),
            None,
            Some(&record),
            &stem,
            &tx,
            &stop,
        )
        .await
        .expect("recorded");
        let size = std::fs::metadata(&file).expect("file").len();
        let p = tx.borrow().clone();
        println!(
            "{} bytes, {:.0} s recorded, {} shown",
            size, p.recorded, p.received
        );
        assert!(
            p.recorded > 40.0,
            "recorded on after the stop: {:.0} s",
            p.recorded
        );
        assert!(size >= p.received);
        std::fs::remove_dir_all(&out).ok();
    }
}
