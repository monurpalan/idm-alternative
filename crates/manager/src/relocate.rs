//! Moving a download's files on disk: a rename on the same drive, and onto another drive a
//! copy that is checked before the original is deleted.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often a copy reports how far it is.
const REPORT_EVERY: Duration = Duration::from_millis(250);

/// The engine's files next to a download's path, moved along with it.
const SIBLINGS: [&str; 3] = [".shardload-part", ".shardload-state", ".shardload-hls"];

/// What belongs to the download at `path` and exists: the file (or folder) itself, the
/// engine's partial data and state, and for a stream or a video page (`stream`) the files
/// named after it, such as subtitles and yt-dlp's partial files.
pub(crate) fn members(path: &Path, stream: bool) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::iter::once(path.to_owned())
        .chain(SIBLINGS.iter().map(|s| with_suffix(path, s)))
        .filter(|p| p.exists())
        .collect();
    if stream && let (Some(dir), Some(stem)) = (path.parent(), path.file_stem()) {
        let prefix = format!("{}.", stem.to_string_lossy());
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let partial = name.ends_with(".part") || name.ends_with(".ytdl");
            let subtitles = name.ends_with(".vtt") || name.ends_with(".ttml");
            if name.starts_with(&prefix) && (partial || subtitles) {
                out.push(entry.path());
            }
        }
    }
    out
}

/// Where `member` of the download at `from` goes when the download goes to `to`: the same
/// name with the old file name (or, for files named after the stem, the old stem) replaced.
pub(crate) fn destination(member: &Path, from: &Path, to: &Path) -> PathBuf {
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let stem = |p: &Path| {
        p.file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let dir = to.parent().unwrap_or(to);
    let member_name = name(member);
    let rest = match member_name.strip_prefix(&name(from)) {
        Some(rest) => format!("{}{rest}", name(to)),
        None => match member_name.strip_prefix(&stem(from)) {
            Some(rest) => format!("{}{rest}", stem(to)),
            None => member_name,
        },
    };
    dir.join(rest)
}

/// The same place on disk as far as the file system tells: Windows ignores case.
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    PathBuf::from(p)
}

/// Bytes in a file, or in every file under a folder.
pub(crate) fn size_of(path: &Path) -> u64 {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| size_of(&e.path()))
            .sum(),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// Moves each `(from, to)`; `to` must not exist. On the same drive a rename; otherwise
/// (or with `copy`) a copy, checked against the original, then the original is deleted.
/// `progress(copied, total)` is called now and then while copying. When one fails, those
/// already moved are moved back.
pub(crate) fn move_all(
    moves: &[(PathBuf, PathBuf)],
    copy: bool,
    mut progress: impl FnMut(u64, u64),
) -> io::Result<()> {
    for (from, to) in moves {
        // Only the case of the name changes: the same file.
        if !same_path(from, to) && fs::symlink_metadata(to).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} already exists", to.display()),
            ));
        }
    }
    let total: u64 = moves.iter().map(|(from, _)| size_of(from)).sum();
    let mut copier = Copier {
        copied: 0,
        total,
        reported: Instant::now(),
        progress: &mut progress,
    };
    for (i, (from, to)) in moves.iter().enumerate() {
        if let Err(e) = move_one(from, to, copy, &mut copier) {
            for (from, to) in moves[..i].iter().rev() {
                let mut back = Copier {
                    copied: 0,
                    total: 0,
                    reported: Instant::now(),
                    progress: &mut |_, _| {},
                };
                if let Err(e) = move_one(to, from, copy, &mut back) {
                    tracing::warn!("could not move {} back: {e}", to.display());
                }
            }
            return Err(e);
        }
    }
    if copier.copied > 0 {
        (copier.progress)(copier.copied, copier.total);
    }
    Ok(())
}

struct Copier<'a> {
    copied: u64,
    total: u64,
    reported: Instant,
    progress: &'a mut dyn FnMut(u64, u64),
}

impl Copier<'_> {
    fn add(&mut self, n: u64) {
        self.copied += n;
        if self.reported.elapsed() >= REPORT_EVERY {
            self.reported = Instant::now();
            (self.progress)(self.copied, self.total);
        }
    }
}

fn move_one(from: &Path, to: &Path, copy: bool, copier: &mut Copier) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    if !copy {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {}
            Err(e) => return Err(e),
        }
    }
    if let Err(e) = copy_tree(from, to, copier) {
        // Nothing half-copied is left behind; the original stays.
        if to.is_dir() {
            fs::remove_dir_all(to).ok();
        } else {
            fs::remove_file(to).ok();
        }
        return Err(e);
    }
    if from.is_dir() {
        fs::remove_dir_all(from)
    } else {
        fs::remove_file(from)
    }
}

fn copy_tree(from: &Path, to: &Path, copier: &mut Copier) -> io::Result<()> {
    if fs::symlink_metadata(from)?.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()), copier)?;
        }
        return Ok(());
    }
    copy_file(from, to, copier)
}

/// Copies one file, flushed to disk and checked to be as long as the original, keeping its
/// modified time and (on Windows) its Mark of the Web.
fn copy_file(from: &Path, to: &Path, copier: &mut Copier) -> io::Result<()> {
    let mut src = File::open(from)?;
    let meta = src.metadata()?;
    let mut dst = OpenOptions::new().write(true).create_new(true).open(to)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])?;
        copier.add(n as u64);
    }
    if let Ok(modified) = meta.modified() {
        dst.set_modified(modified).ok();
    }
    dst.sync_all()?;
    let copied = dst.metadata()?.len();
    if copied != meta.len() {
        return Err(io::Error::other(format!(
            "the copy of {} has {copied} bytes instead of {}",
            from.display(),
            meta.len()
        )));
    }
    #[cfg(windows)]
    if let Ok(zone) = fs::read(with_suffix(from, ":Zone.Identifier")) {
        fs::write(with_suffix(to, ":Zone.Identifier"), zone).ok();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join("shardload-relocate-unit")
            .join(format!("{name}-{}", std::process::id()));
        fs::remove_dir_all(&d).ok();
        fs::create_dir_all(&d).expect("create");
        d
    }

    #[test]
    fn members_and_their_new_names() {
        let d = scratch("members");
        let video = d.join("film.mp4");
        for name in [
            "film.mp4.shardload-part",
            "film.mp4.shardload-state",
            "film.tr.vtt",
            "film.f137.mp4.part",
            "filmography.txt",
        ] {
            fs::write(d.join(name), b"x").expect("write");
        }
        let mut got = members(&video, true);
        got.sort();
        let mut want = vec![
            d.join("film.f137.mp4.part"),
            d.join("film.mp4.shardload-part"),
            d.join("film.mp4.shardload-state"),
            d.join("film.tr.vtt"),
        ];
        want.sort();
        assert_eq!(got, want);
        // Not a stream: only the engine's own files.
        assert_eq!(members(&video, false).len(), 2);

        let to = d.join("sub").join("movie.mp4");
        let dest = |n: &str| destination(&d.join(n), &video, &to);
        assert_eq!(
            dest("film.mp4.shardload-part"),
            d.join("sub").join("movie.mp4.shardload-part")
        );
        assert_eq!(dest("film.tr.vtt"), d.join("sub").join("movie.tr.vtt"));
        assert_eq!(
            dest("film.f137.mp4.part"),
            d.join("sub").join("movie.f137.mp4.part")
        );
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn copies_files_and_folders_then_deletes_the_originals() {
        let d = scratch("copy");
        let file = d.join("a.bin");
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(&file, &data).expect("write");
        let folder = d.join("album");
        fs::create_dir_all(folder.join("cd2")).expect("mkdir");
        fs::write(folder.join("1.flac"), b"one").expect("write");
        fs::write(folder.join("cd2").join("2.flac"), b"two").expect("write");

        let other = d.join("other drive");
        let moves = vec![
            (file.clone(), other.join("b.bin")),
            (folder.clone(), other.join("album (1)")),
        ];
        let mut last = (0, 0);
        move_all(&moves, true, |c, t| last = (c, t)).expect("move");
        assert!(!file.exists() && !folder.exists());
        assert_eq!(fs::read(other.join("b.bin")).expect("read"), data);
        assert_eq!(
            fs::read(other.join("album (1)").join("cd2").join("2.flac")).expect("read"),
            b"two"
        );
        let total = data.len() as u64 + 6;
        assert_eq!(last, (total, total));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_taken_destination_moves_nothing() {
        let d = scratch("taken");
        fs::write(d.join("a"), b"a").expect("write");
        fs::write(d.join("b"), b"b").expect("write");
        fs::write(d.join("c"), b"c").expect("write");
        let moves = vec![(d.join("a"), d.join("x")), (d.join("b"), d.join("c"))];
        assert!(move_all(&moves, false, |_, _| {}).is_err());
        assert!(d.join("a").exists() && !d.join("x").exists());
        assert_eq!(fs::read(d.join("c")).expect("read"), b"c");
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_failed_move_puts_back_what_moved() {
        let d = scratch("rollback");
        fs::write(d.join("a"), b"a").expect("write");
        // The second source is missing, so its move fails after the first succeeded.
        let moves = vec![(d.join("a"), d.join("x")), (d.join("missing"), d.join("y"))];
        assert!(move_all(&moves, true, |_, _| {}).is_err());
        assert_eq!(fs::read(d.join("a")).expect("read"), b"a");
        assert!(!d.join("x").exists());
        fs::remove_dir_all(&d).ok();
    }
}
