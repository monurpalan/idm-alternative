//! The per-download state file that makes resume possible.
//!
//! Checkpoints save positions captured before the state is written, without flushing the
//! data file (ADR 0002): bytes handed to the OS survive the process crashing or being killed,
//! so then the state never claims missing bytes. After an OS crash or power loss it may claim
//! the last few seconds of bytes that never reached the disk. Only stopping early (pause,
//! cancel, an error) flushes the data first. The state file itself is always on the disk
//! before it replaces the previous one.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::remote::Validators;
use crate::segments::SavedSegment;

const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// The URL the user asked for (not the redirect target).
    pub url: String,
    pub size: u64,
    pub validators: Validators,
    pub segments: Vec<SavedSegment>,
}

impl State {
    pub fn new(url: &str, size: u64, validators: Validators, segments: Vec<SavedSegment>) -> Self {
        Self {
            version: VERSION,
            url: url.to_owned(),
            size,
            validators,
            segments,
        }
    }

    /// Loads a state file; anything unreadable or from another version counts as absent.
    pub fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let state: State = serde_json::from_str(&text).ok()?;
        let consistent = state.version == VERSION
            && state
                .segments
                .iter()
                .all(|s| s.start <= s.written && s.written <= s.end && s.end <= state.size);
        consistent.then_some(state)
    }

    /// Writes atomically and durably: the new file is on the disk before it replaces the old
    /// one, so a crash or a power loss leaves either the old or the new file, never half of
    /// one. A few KB: the flush costs nothing worth measuring (unlike the data file's).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        let tmp = sibling(path, ".tmp");
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&serde_json::to_vec(self).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        // The rename itself is in the folder: on Unix it is made durable by syncing the
        // folder. Windows has no such call for folders; NTFS journals the rename.
        #[cfg(unix)]
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    }
}

/// `path` with `suffix` appended to its file name.
pub fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(written: u64) -> State {
        State::new(
            "https://e.x/f",
            100,
            Validators::default(),
            vec![SavedSegment {
                start: 0,
                written,
                end: 100,
            }],
        )
    }

    #[test]
    fn saved_states_replace_each_other_whole() {
        let dir = std::env::temp_dir().join(format!("shardload-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("f.shardload-state");
        state(10).save(&path).expect("save");
        assert_eq!(State::load(&path).expect("load").segments[0].written, 10);
        state(60).save(&path).expect("save again");
        assert_eq!(State::load(&path).expect("load").segments[0].written, 60);
        assert!(!sibling(&path, ".tmp").exists(), "no temporary file left");
        std::fs::remove_dir_all(&dir).ok();
    }
}
