//! Which folders a local tool may save into. Downloads go to the download folder or to a
//! category's folder; a tool may pick a folder inside one of those, but nothing outside them,
//! so a tool that is tricked into asking for another place cannot write there.

use std::path::{Component, Path, PathBuf};

/// Whether `dir` is one of `roots` or lies inside one. Only absolute paths without `..` count;
/// links and junctions are followed for the part of the path that exists.
pub fn within(dir: &Path, roots: &[PathBuf]) -> bool {
    let Some(dir) = resolve(dir).map(key) else {
        return false;
    };
    roots
        .iter()
        .filter_map(|root| resolve(root).map(key))
        .any(|root| dir.starts_with(root))
}

/// `path` as an absolute path with its existing part resolved through links; `None` for a
/// relative path or one with a `..` part.
fn resolve(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return None;
    }
    // The parts that do not exist yet cannot be links, so they are added back as written.
    let mut missing = Vec::new();
    let mut existing = path;
    while !existing.exists() {
        missing.push(existing.file_name()?);
        existing = existing.parent()?;
    }
    let mut resolved = existing.canonicalize().ok()?;
    for part in missing.iter().rev() {
        resolved.push(part);
    }
    Some(resolved)
}

/// Windows file names ignore case, so the comparison does too.
fn key(path: PathBuf) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::within;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A fresh folder for one test, holding a `downloads` folder.
    fn scratch(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("idma-folder-{}-{name}", std::process::id()));
        fs::remove_dir_all(&base).ok();
        fs::create_dir_all(base.join("downloads")).expect("create");
        base
    }

    #[test]
    fn allows_a_root_and_folders_inside_it() {
        let base = scratch("inside");
        let root = base.join("downloads");
        let roots = [root.clone()];
        assert!(within(&root, &roots));
        assert!(
            within(&root.join("new"), &roots),
            "a folder that does not exist yet"
        );
        assert!(within(&root.join("iso").join("2026"), &roots));
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn refuses_other_folders() {
        let base = scratch("outside");
        let root = base.join("downloads");
        let roots = [root.clone()];
        assert!(!within(&base, &roots), "the folder above a root");
        assert!(
            !within(&base.join("downloads-old"), &roots),
            "a sibling with the same start"
        );
        assert!(
            !within(&root.join("..").join("downloads").join("x"), &roots),
            "a `..` part is refused even when it ends back inside"
        );
        assert!(!within(Path::new("downloads"), &roots), "a relative path");
        fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn follows_links_out_of_a_root() {
        let base = scratch("link");
        let root = base.join("downloads");
        let outside = base.join("elsewhere");
        fs::create_dir_all(&outside).expect("create");
        std::os::unix::fs::symlink(&outside, root.join("link")).expect("symlink");
        assert!(!within(&root.join("link"), std::slice::from_ref(&root)));
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn no_roots_refuse_everything() {
        let base = scratch("none");
        assert!(!within(&base.join("downloads"), &[]));
        fs::remove_dir_all(&base).ok();
    }
}
