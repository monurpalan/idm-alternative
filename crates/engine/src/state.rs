//! The per-download state file that makes resume possible.
//!
//! It is only ever written after the data it describes has been flushed to disk, so after a
//! crash or power loss it may lag behind the file, but never claims bytes that are missing.

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

    /// Writes atomically: a crash leaves either the old or the new file, never half of one.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = sibling(path, ".tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(self).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(&tmp, path)
    }
}

/// `path` with `suffix` appended to its file name.
pub fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}
