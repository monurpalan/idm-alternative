//! Unpacking a finished archive into a folder next to it: ZIP (stored and deflate entries),
//! and tar, plain or compressed with gzip or xz. 7z and RAR are not unpacked: nothing in the
//! program reads them.
//!
//! Archives are untrusted: names that climb out of the folder are skipped (and said), links
//! inside a tar are not made, and an archive that says it holds more than the drive has free
//! is not started.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use shardload_engine::LocalZip;

/// The memory an xz archive may ask for to be unpacked (its dictionary), in KiB: what xz's
/// highest preset needs, with room. One that asks for more is refused rather than read.
const XZ_MEMORY_KB: u32 = 256 << 10;

/// What unpacking an archive came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unpacked {
    pub folder: PathBuf,
    pub files: usize,
    /// Entries left out, each with why.
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Zip,
    Tar,
    TarGz,
    TarXz,
}

fn kind(name: &str) -> Option<Kind> {
    let n = name.to_ascii_lowercase();
    Some(if n.ends_with(".zip") {
        Kind::Zip
    } else if n.ends_with(".tar") {
        Kind::Tar
    } else if n.ends_with(".tar.gz") || n.ends_with(".tgz") {
        Kind::TarGz
    } else if n.ends_with(".tar.xz") || n.ends_with(".txz") {
        Kind::TarXz
    } else {
        return None;
    })
}

/// Whether a file of this name is an archive that can be unpacked.
pub fn can_unpack(name: &str) -> bool {
    kind(name).is_some()
}

/// Unpacks `file` into `folder` (made here). Runs on the calling thread: call it from a
/// blocking task.
pub(crate) fn unpack(file: &Path, folder: &Path, free: Option<u64>) -> Result<Unpacked, String> {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let kind = kind(&name).ok_or("not an archive that can be unpacked (ZIP or tar)")?;
    let stop = AtomicBool::new(false);
    match kind {
        Kind::Zip => {
            let mut zip = LocalZip::open(file).map_err(|e| e.to_string())?;
            let total: u64 = zip.entries().iter().map(|e| e.size).sum();
            room(total, free)?;
            std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
            let results = zip.unpack(folder, &stop).map_err(|e| e.to_string())?;
            let skipped: Vec<String> = results
                .iter()
                .filter_map(|r| r.error.as_ref().map(|e| format!("{}: {e}", r.name)))
                .collect();
            let files = results
                .iter()
                .filter(|r| r.error.is_none() && !r.name.ends_with('/'))
                .count();
            Ok(Unpacked {
                folder: folder.to_owned(),
                files,
                skipped,
            })
        }
        Kind::Tar | Kind::TarGz | Kind::TarXz => {
            let open = || std::fs::File::open(file).map(std::io::BufReader::new);
            let reader = || -> std::io::Result<Box<dyn Read>> {
                Ok(match kind {
                    Kind::TarGz => Box::new(flate2::read::MultiGzDecoder::new(open()?)),
                    Kind::TarXz => Box::new(lzma_rust2::XzReader::new_mem_limit(
                        open()?,
                        true,
                        XZ_MEMORY_KB,
                    )),
                    _ => Box::new(open()?),
                })
            };
            // A first pass for the sizes, so a bomb is found before anything is written.
            let total = tar_size(reader().map_err(|e| e.to_string())?)?;
            room(total, free)?;
            std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
            untar(reader().map_err(|e| e.to_string())?, folder)
        }
    }
}

/// Fails when `total` bytes would not fit in `free`.
fn room(total: u64, free: Option<u64>) -> Result<(), String> {
    match free {
        Some(free) if total > free => Err(format!(
            "it holds {} MiB, more than the {} MiB free on the drive",
            total >> 20,
            free >> 20
        )),
        _ => Ok(()),
    }
}

fn tar_size(r: Box<dyn Read>) -> Result<u64, String> {
    let mut archive = tar::Archive::new(r);
    let mut total = 0u64;
    for entry in archive.entries().map_err(damaged)? {
        let entry = entry.map_err(damaged)?;
        total = total.saturating_add(entry.header().size().unwrap_or(0));
    }
    Ok(total)
}

fn untar(r: Box<dyn Read>, folder: &Path) -> Result<Unpacked, String> {
    let mut archive = tar::Archive::new(r);
    archive.set_preserve_permissions(false);
    archive.set_overwrite(false);
    let mut files = 0;
    let mut skipped = Vec::new();
    for entry in archive.entries().map_err(damaged)? {
        let mut entry = entry.map_err(damaged)?;
        let name = entry
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            if !kind.is_pax_global_extensions() && !kind.is_pax_local_extensions() {
                skipped.push(format!("{name}: links and special files are not made"));
            }
            continue;
        }
        if kind.is_dir() {
            // Made here rather than by the archive: its mode could leave a folder that cannot
            // be written into (no execute bit), and it is not the archive's to choose.
            match shardload_engine::safe_path(&name) {
                Some(p) => std::fs::create_dir_all(folder.join(p)).map_err(|e| e.to_string())?,
                None => skipped.push(format!(
                    "{name}: unsafe name: it would be written outside the folder"
                )),
            }
            continue;
        }
        // `unpack_in` refuses names that climb out of the folder (and says false).
        match entry.unpack_in(folder) {
            Ok(true) => files += 1,
            Ok(false) => skipped.push(format!(
                "{name}: unsafe name: it would be written outside the folder"
            )),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                skipped.push(format!("{name}: a file is already there"));
            }
            Err(e) => return Err(damaged(e)),
        }
    }
    Ok(Unpacked {
        folder: folder.to_owned(),
        files,
        skipped,
    })
}

fn damaged(e: std::io::Error) -> String {
    format!("the archive is damaged: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join("shardload-manager-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).expect("dir");
        d
    }

    /// A tar with a file, a folder, a link and a name that climbs out.
    fn tar_bytes() -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let mut add = |name: &str, data: &[u8], kind: tar::EntryType| {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(kind);
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            if kind == tar::EntryType::Symlink {
                h.set_link_name("/etc/passwd").expect("link");
            }
            // `append_data` would refuse `..`; the raw name is what a hostile archive has.
            let raw = &mut h.as_old_mut().name;
            raw[..name.len()].copy_from_slice(name.as_bytes());
            h.set_cksum();
            b.append(&h, data).expect("append");
        };
        add("top.txt", b"top", tar::EntryType::Regular);
        add("sub/", b"", tar::EntryType::Directory);
        add("sub/inner.bin", &[9u8; 3000], tar::EntryType::Regular);
        add("link", b"", tar::EntryType::Symlink);
        add("../evil.txt", b"x", tar::EntryType::Regular);
        b.into_inner().expect("tar")
    }

    #[test]
    fn unpacks_tar_gz_and_xz() {
        let d = dir("untar");
        let tar = tar_bytes();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &tar).expect("gz");
        let mut xz = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(6))
            .expect("xz");
        std::io::Write::write_all(&mut xz, &tar).expect("xz");
        let xz = xz.finish().expect("xz");
        for (name, bytes) in [
            ("a.tar", tar.clone()),
            ("b.tar.gz", gz.finish().expect("gz")),
            ("c.txz", xz),
        ] {
            let file = d.join(name);
            std::fs::write(&file, bytes).expect("write");
            let out = crate::after::unpack_folder(&file);
            let u = unpack(&file, &out, None).expect(name);
            assert_eq!(u.files, 2, "{name}: {u:?}");
            assert_eq!(u.skipped.len(), 2, "{name}: {u:?}");
            assert_eq!(std::fs::read(out.join("top.txt")).expect("top"), b"top");
            assert_eq!(
                std::fs::read(out.join("sub/inner.bin")).expect("inner"),
                [9u8; 3000]
            );
            assert!(!out.join("link").exists());
            assert!(!d.join("evil.txt").exists());
        }
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn refuses_what_does_not_fit_or_is_damaged() {
        let d = dir("untar-bad");
        let file = d.join("a.tar");
        std::fs::write(&file, tar_bytes()).expect("write");
        let err = unpack(&file, &d.join("out"), Some(100)).expect_err("too big");
        assert!(err.contains("free"), "{err}");
        assert!(!d.join("out").exists(), "nothing written");
        let gz = d.join("b.tar.gz");
        std::fs::write(&gz, b"\x1f\x8b\x08\x00garbage that is not gzip").expect("write");
        assert!(unpack(&gz, &d.join("out2"), None).is_err());
        assert!(unpack(&d.join("x.rar"), &d.join("out3"), None).is_err());
        // A flipped byte inside xz data: its check says so.
        let mut xz = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(6))
            .expect("xz");
        std::io::Write::write_all(&mut xz, &tar_bytes()).expect("xz");
        let mut xz = xz.finish().expect("xz");
        let middle = xz.len() / 2;
        xz[middle] ^= 0x55;
        let file = d.join("c.tar.xz");
        std::fs::write(&file, xz).expect("write");
        assert!(unpack(&file, &d.join("out4"), None).is_err());
        std::fs::remove_dir_all(&d).ok();
    }
}
