//! Disk space for the queue: what the downloads about to run need on each drive against what
//! is free there, and a watch while they run that pauses them before a drive fills up.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use crate::task::{Status, Task, TaskId};

/// Kept free on every drive: downloads pause before going below it, and a queue that would
/// leave less is short.
pub const RESERVE: u64 = 500 << 20;

/// The drive (volume) a folder is on and the bytes free there; `None` when it cannot be read.
/// Replaceable in [`crate::Config`], so tests need not fill a real disk.
pub type FreeSpace = Arc<dyn Fn(&Path) -> Option<(PathBuf, u64)> + Send + Sync>;

/// The real one: the folder's volume (its drive on Windows, its mount elsewhere) and the space
/// free to this user there.
pub fn free_space(dir: &Path) -> Option<(PathBuf, u64)> {
    // A folder not made yet is on the drive of the nearest one that is.
    let existing = dir.ancestors().find(|p| p.exists())?;
    let free = fs4::available_space(existing).ok()?;
    Some((volume_of(existing), free))
}

#[cfg(windows)]
fn volume_of(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            _ => break,
        }
    }
    out
}

#[cfg(not(windows))]
fn volume_of(path: &Path) -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    let Ok(dev) = std::fs::metadata(path).map(|m| m.dev()) else {
        return PathBuf::from("/");
    };
    // The mount point: the highest folder still on the same device.
    let mut top = path.to_path_buf();
    for a in path.ancestors().skip(1) {
        match std::fs::metadata(a) {
            Ok(m) if m.dev() == dev => top = a.to_path_buf(),
            _ => break,
        }
    }
    top
}

/// What the downloads going to one drive need there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DriveNeed {
    /// `D:\`, or a mount point.
    pub volume: PathBuf,
    /// Bytes still to download of the ones whose size is known.
    pub need: u64,
    /// `None` when it could not be read.
    pub free: Option<u64>,
    /// How many of them have no known size yet (a link not asked yet, a video page, a
    /// torrent before its first start): they need more than `need` says.
    pub unknown: usize,
    pub tasks: Vec<TaskId>,
}

impl DriveNeed {
    /// Bytes missing to download them all and keep [`RESERVE`] free; `None` when there is
    /// room (or the free space is not known).
    pub fn short(&self) -> Option<u64> {
        let free = self.free?;
        (self.need + RESERVE)
            .checked_sub(free)
            .filter(|missing| *missing > 0)
    }
}

/// Where a download's data goes: a torrent's or an archive's own folder, else its folder.
pub(crate) fn target_dir(t: &Task) -> PathBuf {
    let stream = t.stream.as_ref();
    stream
        .and_then(|s| s.torrent.as_ref())
        .and_then(|c| c.folder.clone())
        .or_else(|| {
            stream
                .and_then(|s| s.archive.as_ref())
                .and_then(|c| c.folder.clone())
        })
        .unwrap_or_else(|| t.dir.clone())
}

/// The needs of `tasks` (unfinished ones; finished ones are left out) per drive, in the order
/// the drives first come up.
pub(crate) fn needs(tasks: &[Task], free: &FreeSpace) -> Vec<DriveNeed> {
    let mut out: Vec<DriveNeed> = Vec::new();
    for t in tasks.iter().filter(|t| t.status != Status::Done) {
        let dir = target_dir(t);
        let (volume, space) = match free(&dir) {
            Some((v, f)) => (v, Some(f)),
            None => (dir.clone(), None),
        };
        let i = match out.iter().position(|d| d.volume == volume) {
            Some(i) => i,
            None => {
                out.push(DriveNeed {
                    volume,
                    need: 0,
                    free: space,
                    unknown: 0,
                    tasks: Vec::new(),
                });
                out.len() - 1
            }
        };
        let d = &mut out[i];
        match t.size {
            Some(size) => d.need += size.saturating_sub(t.received),
            None => d.unknown += 1,
        }
        d.tasks.push(t.id);
    }
    out
}

/// A drive ran low while downloads were writing to it: they were paused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiskLow {
    pub volume: PathBuf,
    pub free: u64,
    pub paused: Vec<TaskId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::Category;

    fn task(id: TaskId, dir: &str, size: Option<u64>, received: u64, status: Status) -> Task {
        Task {
            id,
            url: format!("https://e.x/{id}"),
            headers: Vec::new(),
            dir: PathBuf::from(dir),
            name: None,
            path: None,
            size,
            received,
            status,
            error: None,
            error_kind: None,
            category: Category::General,
            connections: 1,
            speed_limit: None,
            checksum: None,
            checksum_from: None,
            added_at: 0,
            finished_at: None,
            queue_pos: id,
            stream: None,
            mirrors: Vec::new(),
            final_url: None,
            group_id: None,
            pieces: None,
            link_at: 0,
            content: None,
            warning: None,
            name_given: false,
            page_title: None,
            page_checksum: None,
        }
    }

    /// `d/...` is on a drive with 10 GB free, `e/...` on one with 100 GB.
    fn fake() -> FreeSpace {
        Arc::new(|dir: &Path| {
            let first = dir
                .components()
                .next()?
                .as_os_str()
                .to_string_lossy()
                .into_owned();
            let free = match first.as_str() {
                "d" => 10 << 30,
                "e" => 100 << 30,
                _ => return None,
            };
            Some((PathBuf::from(first), free))
        })
    }

    #[test]
    fn needs_per_drive() {
        const GB: u64 = 1 << 30;
        let tasks = [
            task(1, "d/films", Some(8 * GB), 0, Status::Queued),
            task(2, "d/iso", Some(6 * GB), 2 * GB, Status::Paused),
            task(3, "e/x", Some(GB), 0, Status::Queued),
            task(4, "d/films", None, 0, Status::Queued),
            task(5, "d/done", Some(50 * GB), 50 * GB, Status::Done),
            task(6, "z/elsewhere", Some(GB), 0, Status::Queued),
        ];
        let needs = needs(&tasks, &fake());
        assert_eq!(needs.len(), 3);
        let d = &needs[0];
        assert_eq!(d.volume, PathBuf::from("d"));
        assert_eq!(d.need, 12 * GB, "8 to go, and 4 left of the paused one");
        assert_eq!(d.unknown, 1);
        assert_eq!(d.tasks, vec![1, 2, 4], "not the finished one");
        assert_eq!(d.short(), Some(2 * GB + RESERVE));
        let e = &needs[1];
        assert_eq!((e.need, e.short()), (GB, None));
        let z = &needs[2];
        assert_eq!((z.free, z.short()), (None, None), "unreadable: not short");
    }

    #[test]
    fn the_real_free_space() {
        let dir = std::env::temp_dir();
        let (volume, free) = free_space(&dir).expect("free space of the temp folder");
        assert!(free > 0);
        assert!(dir.starts_with(&volume), "{dir:?} on {volume:?}");
        // A folder not made yet: the drive of the nearest existing one.
        let (later, _) = free_space(&dir.join("not/made/yet")).expect("free space");
        assert_eq!(later, volume);
    }
}
