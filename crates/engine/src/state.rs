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

/// After an attempt that did not stop cleanly, each segment's last this many seconds of data
/// are downloaded again (at its speed when saved), and at least [`REWIND_MIN`].
const REWIND_SECS: u64 = 2;
const REWIND_MIN: u64 = 1 << 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// The URL the user asked for (not the redirect target).
    pub url: String,
    pub size: u64,
    pub validators: Validators,
    pub segments: Vec<SavedSegment>,
    /// Saved when the download stopped early (pause, cancel, an error), after the data was
    /// flushed to the disk: every byte it counts is there. A checkpoint while downloading is
    /// not clean: after a power loss its last seconds may be missing (ADR 0002).
    #[serde(default)]
    pub clean: bool,
    /// A zsync download (see [`crate::zsync`]): its segments are the ranges still to download;
    /// what is outside them came from the older copies already.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zsync: Option<ZsyncState>,
}

/// What a zsync download keeps besides its ranges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZsyncState {
    /// Which `.zsync` it follows: another one is another file.
    pub control: String,
    /// Bytes taken from the older copies, and the copy most came from.
    pub reused: u64,
    pub from: Option<PathBuf>,
}

impl State {
    pub fn new(url: &str, size: u64, validators: Validators, segments: Vec<SavedSegment>) -> Self {
        Self {
            version: VERSION,
            url: url.to_owned(),
            size,
            validators,
            segments,
            clean: false,
            zsync: None,
        }
    }

    /// Loads a state file; anything unreadable or from another version counts as absent. A
    /// state not saved cleanly (see [`State::clean`]) comes back with each segment's last
    /// seconds taken off, so they are downloaded again: bytes that may never have reached the
    /// disk are not trusted. Without it, a power loss could leave zeros in a "finished" file.
    pub fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut state: State = serde_json::from_str(&text).ok()?;
        if !state.clean {
            state.rewind();
            // What is left can be trusted: saved again as it is (a new link for the download
            // keeps the state), it must not be rewound a second time.
            state.clean = true;
        }
        let consistent = state.version == VERSION
            && state
                .segments
                .iter()
                .all(|s| s.start <= s.written && s.written <= s.end && s.end <= state.size);
        consistent.then_some(state)
    }

    /// Takes each segment's last seconds off what it counts as written.
    fn rewind(&mut self) {
        for s in &mut self.segments {
            let back = (s.speed * REWIND_SECS).max(REWIND_MIN);
            s.written = s.written.saturating_sub(back).max(s.start);
        }
    }

    /// Writes atomically and durably: the new file is on the disk before it replaces the old
    /// one, so a crash or a power loss leaves either the old or the new file, never half of
    /// one. A few KB, but a slow disk can take a while to confirm the flush.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        self.save_unless(path, &std::sync::Mutex::new(false))
    }

    /// [`State::save`], unless `finished` is set once the new file is on the disk: the
    /// download ended meanwhile and its state file is gone, so it must not come back. The
    /// flush happens outside the lock; only the last step (the rename) holds it.
    pub fn save_unless(
        &self,
        path: &Path,
        finished: &std::sync::Mutex<bool>,
    ) -> std::io::Result<()> {
        use std::io::Write;
        let tmp = sibling(path, ".tmp");
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&serde_json::to_vec(self).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        drop(file);
        let finished = finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *finished {
            std::fs::remove_file(&tmp).ok();
            return Ok(());
        }
        std::fs::rename(&tmp, path)?;
        drop(finished);
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
                speed: 0,
            }],
        )
    }

    #[test]
    fn an_unclean_state_comes_back_with_its_last_seconds_taken_off() {
        const MB: u64 = 1 << 20;
        let dir = std::env::temp_dir().join(format!("shardload-rewind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("f.shardload-state");
        let seg = |start, written, end, speed| SavedSegment {
            start,
            written,
            end,
            speed,
        };
        let mut s = State::new(
            "https://e.x/f",
            100 * MB,
            Validators::default(),
            vec![
                // 3 MB/s: two seconds back.
                seg(0, 20 * MB, 40 * MB, 3 * MB),
                // Slow: at least 1 MiB back.
                seg(40 * MB, 45 * MB, 60 * MB, 1000),
                // Barely started: not back past its start.
                seg(60 * MB, 60 * MB + 1000, 80 * MB, 2 * MB),
                // Finished: its tail is downloaded again too.
                seg(80 * MB, 100 * MB, 100 * MB, MB),
            ],
        );
        s.save(&path).expect("save");
        let got: Vec<u64> = State::load(&path)
            .expect("load")
            .segments
            .iter()
            .map(|s| s.written)
            .collect();
        assert_eq!(got, vec![14 * MB, 44 * MB, 60 * MB, 98 * MB]);
        // Loaded and saved again unchanged: not rewound twice.
        State::load(&path).expect("load").save(&path).expect("save");
        assert_eq!(
            State::load(&path).expect("load").segments[0].written,
            14 * MB
        );

        // Saved cleanly (stopped with the data flushed): trusted as it is.
        s.clean = true;
        s.save(&path).expect("save");
        let got: Vec<u64> = State::load(&path)
            .expect("load")
            .segments
            .iter()
            .map(|s| s.written)
            .collect();
        assert_eq!(got, vec![20 * MB, 45 * MB, 60 * MB + 1000, 100 * MB]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_save_that_ends_after_the_download_leaves_no_state() {
        let dir = std::env::temp_dir().join(format!("shardload-unless-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("f.shardload-state");
        state(10)
            .save_unless(&path, &std::sync::Mutex::new(true))
            .expect("save");
        assert!(!path.exists() && !sibling(&path, ".tmp").exists());
        state(10)
            .save_unless(&path, &std::sync::Mutex::new(false))
            .expect("save");
        assert!(path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saved_states_replace_each_other_whole() {
        let dir = std::env::temp_dir().join(format!("shardload-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("f.shardload-state");
        let clean = |w| {
            let mut s = state(w);
            s.clean = true;
            s
        };
        clean(10).save(&path).expect("save");
        assert_eq!(State::load(&path).expect("load").segments[0].written, 10);
        clean(60).save(&path).expect("save again");
        assert_eq!(State::load(&path).expect("load").segments[0].written, 60);
        assert!(!sibling(&path, ".tmp").exists(), "no temporary file left");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_state_that_does_not_add_up_is_ignored() {
        let dir = std::env::temp_dir().join(format!("shardload-bad-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("f.shardload-state");
        let load = |s: State| {
            s.save(&path).expect("save");
            State::load(&path)
        };
        let seg = |start, written, end| SavedSegment {
            start,
            written,
            end,
            speed: 0,
        };
        // Saved cleanly, so loading changes nothing in them.
        let with = |segment| {
            let mut s = State::new("https://e.x/f", 100, Validators::default(), vec![segment]);
            s.clean = true;
            s
        };
        assert!(load(with(seg(0, 10, 100))).is_some());
        let mut other = with(seg(0, 10, 100));
        other.version = VERSION + 1;
        assert!(load(other).is_none(), "another version of the format");
        // Written before its start, past its end; a segment past the end of the file.
        for bad in [seg(20, 10, 100), seg(0, 110, 100), seg(0, 10, 120)] {
            assert!(load(with(bad)).is_none(), "{bad:?}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
