//! Unpacking what a helper program's release ships (a zip, or ffmpeg's `bin` folder from a
//! zip or `.tar.xz`), and putting a fresh folder in place of the old one.

use std::path::Path;

/// Puts the folder `fresh` in the place of `folder`, which need not exist: the old one is
/// moved aside first and removed after, so a failed swap leaves it working.
pub(super) fn swap_in(fresh: &Path, folder: &Path) -> Result<(), String> {
    let old = folder.with_extension("old");
    std::fs::remove_dir_all(&old).ok();
    if folder.exists() {
        std::fs::rename(folder, &old).map_err(|e| format!("{e} (in use?)"))?;
    }
    if let Err(e) = std::fs::rename(fresh, folder) {
        std::fs::rename(&old, folder).ok();
        std::fs::remove_dir_all(fresh).ok();
        return Err(e.to_string());
    }
    std::fs::remove_dir_all(&old).ok();
    Ok(())
}

pub(super) fn make_runnable(_file: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(_file, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Whether an archive member is one to keep: a file directly in a `bin` folder, but not
/// ffplay (a player, not needed to join files).
fn wanted(path: &Path) -> Option<&std::ffi::OsStr> {
    let name = path.file_name()?;
    let parent = path.parent()?.file_name()?;
    let is_player = name
        .to_string_lossy()
        .to_ascii_lowercase()
        .starts_with("ffplay");
    (parent == "bin" && !is_player).then_some(name)
}

/// Unpacks every file of a zip into `to`, keeping its folders, and returns how many there
/// were. Members whose names would leave `to` are skipped.
pub(super) fn unpack_all(archive: &Path, to: &Path) -> Result<usize, String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut count = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        if !entry.is_file() {
            continue;
        }
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        let target = to.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = std::fs::File::create(&target).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        make_runnable(&target)?;
        count += 1;
    }
    if count == 0 {
        return Err("the archive is empty".into());
    }
    Ok(count)
}

/// Copies the programs in the `bin` folder of a `.zip` or `.tar.xz` archive into `to`, and
/// returns how many there were.
pub(super) fn unpack_bin(archive: &Path, to: &Path) -> Result<usize, String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut count = 0;
    let mut keep = |path: &Path, from: &mut dyn std::io::Read| -> Result<(), String> {
        let Some(name) = wanted(path) else {
            return Ok(());
        };
        let target = to.join(name);
        let mut out = std::fs::File::create(&target).map_err(|e| e.to_string())?;
        std::io::copy(from, &mut out).map_err(|e| e.to_string())?;
        make_runnable(&target)?;
        count += 1;
        Ok(())
    };
    if archive.to_string_lossy().ends_with(".zip.download")
        || archive.extension().is_some_and(|e| e == "zip")
    {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
            if !entry.is_file() {
                continue;
            }
            let Some(path) = entry.enclosed_name() else {
                continue;
            };
            keep(&path, &mut entry)?;
        }
    } else {
        // xz decoded in a thread, read as a tar through a pipe: the archive is never on disk
        // unpacked whole.
        let (reader, mut writer) = std::io::pipe().map_err(|e| e.to_string())?;
        let decoder = std::thread::spawn(move || {
            let mut input = std::io::BufReader::new(file);
            lzma_rs::xz_decompress(&mut input, &mut writer).map_err(|e| format!("{e:?}"))
        });
        let read = (|| -> Result<(), String> {
            let mut tar = tar::Archive::new(reader);
            for entry in tar.entries().map_err(|e| e.to_string())? {
                let mut entry = entry.map_err(|e| e.to_string())?;
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let path = entry.path().map_err(|e| e.to_string())?.into_owned();
                keep(&path, &mut entry)?;
            }
            Ok(())
        })();
        let decoded = decoder
            .join()
            .map_err(|_| "the xz decoder stopped".to_owned())?;
        read?;
        decoded?;
    }
    if count == 0 {
        return Err("the archive has no programs in a bin folder".into());
    }
    Ok(count)
}
