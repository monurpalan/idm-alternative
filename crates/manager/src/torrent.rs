//! Torrents and magnet links, through librqbit.
//!
//! One librqbit session serves every torrent download, started on first use. A download
//! adds its torrent to the session, reports progress like any other download and takes the
//! torrent out again when it ends, is paused or fails: nothing seeds after a download is
//! done, and starting again adds the torrent anew, which checks what is already on disk and
//! continues from there.
//!
//! The session does not accept incoming connections; it connects out to the peers trackers
//! and the DHT name. The DHT's UDP socket may make Windows ask once whether to allow the app
//! on the network: either answer works, since replies to what we send are let in.
//!
//! File names come from whoever made the torrent, and librqbit writes them as they are. So
//! files go to disk through [`SafeNames`], at the paths [`safe_paths`] gives: the rules of any
//! other download's name, so a part such as `C:x` cannot leave the folder on Windows and a
//! right-to-left override cannot make `fdp.exe` read as `exe.pdf`.

use std::collections::HashSet;
use std::io::IoSlice;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use librqbit::storage::filesystem::FilesystemStorageFactory;
use librqbit::storage::{BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage};
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrentShared, Session,
    SessionOptions, TorrentMetadata,
};
use shardload_engine::{CancellationToken, Phase, Progress};
use tokio::sync::{OnceCell, watch};

/// How long to look for a magnet link's metadata before giving up.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(250);

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone)]
pub struct TorrentConfig {
    /// Find peers through the DHT (needed by most magnet links).
    pub dht: bool,
    /// Announce to the torrent's trackers.
    pub trackers: bool,
    /// Peers to try for every torrent, besides those found (a LAN seed; tests).
    pub peers: Vec<SocketAddr>,
}

impl Default for TorrentConfig {
    fn default() -> Self {
        Self {
            dht: true,
            trackers: true,
            peers: Vec::new(),
        }
    }
}

/// Whether `url` is a torrent: a magnet link, or a `.torrent` file on the web or on disk.
pub fn is_torrent(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with("magnet:") {
        return true;
    }
    let path = lower.split(['?', '#']).next().unwrap_or_default();
    path.ends_with(".torrent")
}

/// What a torrent holds, before downloading it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TorrentInfo {
    pub name: String,
    pub size: u64,
    pub files: Vec<TorrentFile>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TorrentFile {
    /// Path inside the torrent, `/`-separated.
    pub name: String,
    pub size: u64,
}

/// A finished (or stopped) torrent download.
pub struct Done {
    pub size: u64,
}

pub struct Torrents {
    session: OnceCell<Arc<Session>>,
    config: TorrentConfig,
    /// Folder for librqbit's own defaults; every download names its own folder.
    dir: PathBuf,
}

impl Torrents {
    pub fn new(config: TorrentConfig, dir: PathBuf) -> Self {
        Self {
            session: OnceCell::new(),
            config,
            dir,
        }
    }

    async fn session(&self) -> Result<&Arc<Session>> {
        self.session
            .get_or_try_init(|| async {
                let opts = SessionOptions {
                    // No persistence: librqbit would keep the routing table in its own
                    // folder under %LOCALAPPDATA%.
                    dht: self.config.dht.then(|| librqbit::DhtSessionConfig {
                        persistence: None,
                        ..librqbit::DhtSessionConfig::default()
                    }),
                    disable_trackers: !self.config.trackers,
                    listen: None,
                    persistence: None,
                    fastresume: false,
                    disable_local_service_discovery: true,
                    client_name_and_version: Some(format!(
                        "Shardload {}",
                        env!("CARGO_PKG_VERSION")
                    )),
                    ..SessionOptions::default()
                };
                Session::new_with_opts(self.dir.clone(), opts)
                    .await
                    .map_err(|e| format!("could not start the torrent client: {e:#}"))
            })
            .await
    }

    /// The session-wide download limit (the app's global speed limit).
    pub fn set_limit(&self, rate: Option<u64>) {
        if let Some(s) = self.session.get() {
            s.ratelimits.set_download_bps(bps(rate));
        }
    }

    pub async fn close(&self) {
        if let Some(s) = self.session.get() {
            s.stop().await;
        }
    }

    /// Reads the torrent's name and files; for a magnet link this asks peers for them.
    pub async fn inspect(&self, url: &str) -> Result<TorrentInfo> {
        let session = self.session().await?;
        let add = source(url)?;
        let opts = AddTorrentOptions {
            list_only: true,
            initial_peers: self.initial_peers(),
            ..AddTorrentOptions::default()
        };
        let response = tokio::time::timeout(RESOLVE_TIMEOUT, session.add_torrent(add, Some(opts)))
            .await
            .map_err(|_| "no peer sent this torrent's file list in time".to_owned())?
            .map_err(|e| format!("{e:#}"))?;
        let AddTorrentResponse::ListOnly(list) = response else {
            return Err("the torrent client did not list the files".into());
        };
        let paths = safe_paths(list.info.iter_file_details().map(|f| f.filename.to_vec()));
        let files = list
            .info
            .iter_file_details()
            .zip(paths)
            .map(|(f, path)| TorrentFile {
                name: path
                    .iter()
                    .map(|p| p.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
                size: f.len,
            })
            .collect::<Vec<_>>();
        Ok(TorrentInfo {
            name: list.info.name().map(|n| n.to_string()).unwrap_or_default(),
            size: files.iter().map(|f| f.size).sum(),
            files,
        })
    }

    /// Downloads the torrent into `folder` (its files keep their paths inside the torrent),
    /// only `files` (indices into [`TorrentInfo::files`]) when given.
    pub async fn download(
        &self,
        url: &str,
        folder: &Path,
        files: Option<Vec<usize>>,
        limit: Option<u64>,
        progress: &watch::Sender<Progress>,
        cancel: &CancellationToken,
    ) -> Result<Done> {
        let session = self.session().await?;
        let add = source(url)?;
        let opts = AddTorrentOptions {
            output_folder: Some(folder.to_string_lossy().into_owned()),
            only_files: files,
            storage_factory: Some(SafeNames.boxed()),
            // Resuming means writing into the files an earlier attempt left.
            overwrite: true,
            initial_peers: self.initial_peers(),
            ratelimits: librqbit::limits::LimitsConfig {
                download_bps: bps(limit),
                upload_bps: None,
            },
            ..AddTorrentOptions::default()
        };
        progress.send_modify(|p| p.phase = Phase::Probing);
        let added = tokio::select! {
            r = session.add_torrent(add, Some(opts)) => r.map_err(|e| format!("{e:#}"))?,
            () = cancel.cancelled() => return Err(CANCELLED.into()),
        };
        let handle = added
            .into_handle()
            .ok_or("the torrent client did not start the download")?;
        let id = handle.id();
        let result = async {
            loop {
                let stats = handle.stats();
                if let Some(e) = &stats.error {
                    return Err(e.clone());
                }
                let live = stats.live.as_ref();
                progress.send_modify(|p| {
                    p.phase = match stats.state {
                        librqbit::TorrentStatsState::Initializing { .. } => Phase::Verifying,
                        _ => Phase::Downloading,
                    };
                    p.size = Some(stats.total_bytes);
                    p.received = stats.progress_bytes;
                    p.speed = live.map_or(0.0, |l| l.download_speed.as_bytes() as f64);
                    p.connections = live.map_or(0, |l| l.snapshot.peer_stats.live as usize);
                });
                if stats.finished {
                    return Ok(Done {
                        size: stats.total_bytes,
                    });
                }
                tokio::select! {
                    () = tokio::time::sleep(POLL) => {}
                    () = cancel.cancelled() => return Err(CANCELLED.to_owned()),
                }
            }
        }
        .await;
        // Never left running: no seeding, and a later start adds it again.
        if let Err(e) = session.delete(id.into(), false).await {
            tracing::warn!("could not take the torrent out of the session: {e:#}");
        }
        result
    }

    fn initial_peers(&self) -> Option<Vec<SocketAddr>> {
        (!self.config.peers.is_empty()).then(|| self.config.peers.clone())
    }
}

/// The error text of a cancelled torrent download; the manager turns it into
/// [`shardload_engine::Error::Cancelled`].
pub const CANCELLED: &str = "cancelled";

fn bps(rate: Option<u64>) -> Option<NonZeroU32> {
    rate.and_then(|r| NonZeroU32::new(u32::try_from(r).unwrap_or(u32::MAX)))
}

/// Where each of a torrent's files (given as its path parts) goes inside the download folder:
/// every part made a valid file name, and a file whose path comes out the same as an earlier
/// one's (`a?.txt` after `a*.txt`, or `A.txt` after `a.txt`, one file on Windows) numbered.
pub fn safe_paths(files: impl Iterator<Item = Vec<String>>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    files
        .map(|parts| {
            let mut parts: Vec<String> = parts
                .iter()
                .map(|p| shardload_engine::name::sanitize(p))
                .collect();
            let last = parts.pop().unwrap_or_default();
            let path = |name: &str| -> PathBuf {
                parts.iter().map(String::as_str).chain([name]).collect()
            };
            let mut candidate = path(&last);
            let mut n = 2;
            while !seen.insert(candidate.to_string_lossy().to_lowercase()) {
                let (stem, ext) = shardload_engine::name::split_extension(&last);
                candidate = path(&format!("{stem} ({n}){ext}"));
                n += 1;
            }
            candidate
        })
        .collect()
}

/// librqbit's file storage, with the files at the paths [`safe_paths`] gives instead of the
/// torrent's own.
#[derive(Clone, Copy)]
struct SafeNames;

impl StorageFactory for SafeNames {
    type Storage = SafeNamesStorage<Box<dyn TorrentStorage>>;

    fn create(
        &self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<Self::Storage> {
        let inner = FilesystemStorageFactory::default().create(shared, metadata)?;
        Ok(SafeNamesStorage {
            inner: Box::new(inner),
            paths: Vec::new(),
        })
    }

    fn clone_box(&self) -> BoxStorageFactory {
        self.boxed()
    }
}

struct SafeNamesStorage<S> {
    inner: S,
    paths: Vec<PathBuf>,
}

impl<S: TorrentStorage> TorrentStorage for SafeNamesStorage<S> {
    fn init(
        &mut self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        self.paths = safe_paths(
            metadata
                .info
                .iter_file_details()
                .map(|f| f.filename.to_vec()),
        );
        let mut file_infos = metadata.file_infos.clone();
        for (info, path) in file_infos.iter_mut().zip(&self.paths) {
            info.relative_filename.clone_from(path);
        }
        let safe = TorrentMetadata {
            info: metadata.info.clone(),
            torrent_bytes: metadata.torrent_bytes.clone(),
            info_bytes: metadata.info_bytes.clone(),
            file_infos,
        };
        self.inner.init(shared, &safe)
    }

    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.inner.pread_exact(file_id, offset, buf)
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        self.inner.pwrite_all(file_id, offset, buf)
    }

    fn pwrite_all_vectored(
        &self,
        file_id: usize,
        offset: u64,
        bufs: [IoSlice<'_>; 2],
    ) -> anyhow::Result<usize> {
        self.inner.pwrite_all_vectored(file_id, offset, bufs)
    }

    fn remove_file(&self, file_id: usize, filename: &Path) -> anyhow::Result<()> {
        let path = self.paths.get(file_id).map_or(filename, PathBuf::as_path);
        self.inner.remove_file(file_id, path)
    }

    fn remove_directory_if_empty(&self, path: &Path) -> anyhow::Result<()> {
        let safe: PathBuf = path
            .iter()
            .map(|p| shardload_engine::name::sanitize(&p.to_string_lossy()))
            .collect();
        self.inner.remove_directory_if_empty(&safe)
    }

    fn ensure_file_length(&self, file_id: usize, length: u64) -> anyhow::Result<()> {
        self.inner.ensure_file_length(file_id, length)
    }

    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        Ok(Box::new(SafeNamesStorage {
            inner: self.inner.take()?,
            paths: self.paths.clone(),
        }))
    }
}

/// What to hand librqbit: links as they are, a file on disk as its bytes.
fn source(url: &str) -> Result<AddTorrent<'static>> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("magnet:") || lower.starts_with("http://") || lower.starts_with("https://")
    {
        return Ok(AddTorrent::from_url(url.to_owned()));
    }
    let path = url.strip_prefix("file:///").unwrap_or(url);
    std::fs::read(path)
        .map(AddTorrent::from_bytes)
        .map_err(|e| format!("could not read {path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_torrents() {
        assert!(is_torrent("magnet:?xt=urn:btih:abc"));
        assert!(is_torrent("https://x.org/ubuntu.iso.torrent"));
        assert!(is_torrent("https://x.org/a.TORRENT?token=1"));
        assert!(is_torrent(r"C:\Users\me\Downloads\a.torrent"));
        assert!(!is_torrent("https://x.org/a.zip"));
        assert!(!is_torrent("https://x.org/torrent/page"));
    }

    #[test]
    fn file_names_are_made_safe() {
        let raw: &[&[&str]] = &[
            &["Album", "01 Song.mp3"],
            &["C:x", "evil.exe"],
            &["invoice\u{202E}fdp.exe"],
            &["con.txt"],
            &["a?.txt"],
            &["a*.txt"],
            &["A_.TXT"],
            &["notes. "],
        ];
        let safe = safe_paths(
            raw.iter()
                .map(|parts| parts.iter().map(|p| (*p).to_owned()).collect()),
        );
        let want: Vec<PathBuf> = [
            &["Album", "01 Song.mp3"][..],
            &["C_x", "evil.exe"],
            &["invoice_fdp.exe"],
            &["_con.txt"],
            &["a_.txt"],
            &["a_ (2).txt"],
            &["A_ (3).TXT"],
            &["notes"],
        ]
        .iter()
        .map(|parts| parts.iter().collect())
        .collect();
        assert_eq!(safe, want);
    }
}
