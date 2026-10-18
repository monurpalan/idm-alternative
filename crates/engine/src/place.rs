//! Where a download goes on disk: a free name next to existing files, and a lock so two
//! downloads never write the same file.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::state::{State, sibling};

/// What to do when a file with the chosen name already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Conflict {
    /// Save as `name (1).ext`, `name (2).ext`, … (like browsers do).
    #[default]
    Rename,
    /// Replace the existing file when the download finishes.
    Overwrite,
}

/// Compound extensions kept together when numbering: `a (1).tar.gz`, not `a.tar (1).gz`.
const COMPOUND: [&str; 5] = [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz"];

/// The path to download `url` to, as `name` in `dir`.
///
/// An interrupted download of the same URL under this name or one of its numbered variants
/// is picked up again, so running the same download twice resumes instead of starting a
/// copy. Otherwise `conflict` decides what happens when the name is taken. A name that an
/// unfinished download of another URL is using is always skipped.
pub fn target(dir: &Path, name: &str, url: &str, conflict: Conflict) -> PathBuf {
    target_avoiding(dir, name, url, conflict, |_| false)
}

/// [`target`], also skipping paths for which `taken` says yes: those that other downloads
/// were given but have not created on disk yet (e.g. in a download list choosing names for
/// several downloads at once).
pub fn target_avoiding(
    dir: &Path,
    name: &str,
    url: &str,
    conflict: Conflict,
    taken: impl Fn(&Path) -> bool,
) -> PathBuf {
    let base = dir.join(name);
    if conflict == Conflict::Overwrite && !has_other_download(&base, url) && !taken(&base) {
        return base;
    }
    for n in 0u32.. {
        let path = if n == 0 {
            base.clone()
        } else {
            dir.join(numbered(name, n))
        };
        if !taken(&path) && (resumes(&path, url) || !occupied(&path)) {
            return path;
        }
    }
    unreachable!("u32 names exhausted")
}

/// `stem (n).ext`, still within the length limit.
pub fn numbered(name: &str, n: u32) -> String {
    let lower = name.to_ascii_lowercase();
    let (stem, ext) = match COMPOUND.iter().find(|c| lower.ends_with(*c)) {
        Some(c) if name.len() > c.len() => name.split_at(name.len() - c.len()),
        _ => crate::name::split_extension(name),
    };
    let suffix = format!(" ({n}){ext}");
    let room = crate::name::MAX_UNITS.saturating_sub(suffix.encode_utf16().count());
    let mut short = String::new();
    for c in stem.chars() {
        if short.encode_utf16().count() + c.len_utf16() > room {
            break;
        }
        short.push(c);
    }
    format!("{short}{suffix}")
}

/// An interrupted download of `url` is waiting at `path`.
fn resumes(path: &Path, url: &str) -> bool {
    sibling(path, ".shardload-part").exists()
        && State::load(&sibling(path, ".shardload-state")).is_some_and(|s| s.url == url)
}

/// An unfinished download of a different URL is using `path`.
fn has_other_download(path: &Path, url: &str) -> bool {
    sibling(path, ".shardload-part").exists() && !resumes(path, url)
}

fn occupied(path: &Path) -> bool {
    path.exists()
        || sibling(path, ".shardload-part").exists()
        || sibling(path, ".shardload-state").exists()
}

/// Held while a download writes `output`; a second download of the same output fails with
/// [`Error::InUse`] instead of corrupting the first. The OS releases it if the process dies.
pub(crate) struct Lock {
    _file: File,
    path: PathBuf,
}

impl Lock {
    pub(crate) fn acquire(output: &Path) -> Result<Self> {
        let path = sibling(output, ".shardload-lock");
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(TryLockError::WouldBlock) => Err(Error::InUse(output.to_owned())),
            Err(TryLockError::Error(e)) => Err(e.into()),
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::Validators;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join("shardload-engine-unit")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).expect("create");
        d
    }

    fn interrupted(path: &Path, url: &str) {
        std::fs::write(sibling(path, ".shardload-part"), b"x").expect("part");
        State::new(url, 1, Validators::default(), Vec::new())
            .save(&sibling(path, ".shardload-state"))
            .expect("state");
    }

    #[test]
    fn numbering() {
        assert_eq!(numbered("a.zip", 1), "a (1).zip");
        assert_eq!(numbered("a", 2), "a (2)");
        assert_eq!(numbered("src.TAR.GZ", 3), "src (3).TAR.GZ");
        assert_eq!(numbered(".tar.gz", 1), ".tar (1).gz");
        assert_eq!(numbered(".bashrc", 1), ".bashrc (1)");
        let long = format!("{}.zip", "ş".repeat(300));
        let n = numbered(&crate::name::sanitize(&long), 12);
        assert!(n.ends_with(" (12).zip"), "{n}");
        assert!(n.encode_utf16().count() <= crate::name::MAX_UNITS);
    }

    #[test]
    fn free_names_and_resume() {
        let d = dir("target");
        let url = "http://h/a.zip";
        assert_eq!(target(&d, "a.zip", url, Conflict::Rename), d.join("a.zip"));

        std::fs::write(d.join("a.zip"), b"done").expect("write");
        assert_eq!(
            target(&d, "a.zip", url, Conflict::Rename),
            d.join("a (1).zip")
        );
        assert_eq!(
            target(&d, "a.zip", url, Conflict::Overwrite),
            d.join("a.zip")
        );

        // The same URL interrupted as `a (1).zip` is resumed, another URL is not.
        interrupted(&d.join("a (1).zip"), url);
        assert_eq!(
            target(&d, "a.zip", url, Conflict::Rename),
            d.join("a (1).zip")
        );
        assert_eq!(
            target(&d, "a.zip", "http://other/a.zip", Conflict::Rename),
            d.join("a (2).zip")
        );

        // Names handed to other downloads that have not started writing yet are skipped.
        assert_eq!(
            target_avoiding(&d, "c.zip", url, Conflict::Rename, |p| p == d.join("c.zip")),
            d.join("c (1).zip")
        );
        assert_eq!(
            target_avoiding(&d, "c.zip", url, Conflict::Overwrite, |p| p
                == d.join("c.zip")),
            d.join("c (1).zip")
        );

        // Overwrite never takes a name from another URL's unfinished download.
        interrupted(&d.join("b.zip"), "http://other/b.zip");
        assert_eq!(
            target(&d, "b.zip", url, Conflict::Overwrite),
            d.join("b (1).zip")
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn lock_is_exclusive() {
        let d = dir("lock");
        let out = d.join("f.bin");
        let held = Lock::acquire(&out).expect("first");
        assert!(matches!(Lock::acquire(&out), Err(Error::InUse(_))));
        drop(held);
        assert!(!sibling(&out, ".shardload-lock").exists());
        Lock::acquire(&out).expect("after release");
        std::fs::remove_dir_all(&d).ok();
    }
}
