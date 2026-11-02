//! What the fuzz targets share.

use std::path::PathBuf;

/// A file of this process's own for targets whose parser reads from a file.
pub fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!("shardload-fuzz-{}", std::process::id()))
}
