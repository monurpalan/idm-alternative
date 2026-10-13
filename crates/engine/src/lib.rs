//! Download engine for idm-alternative.
//!
//! Planned design:
//! - one preallocated output file written concurrently at offsets,
//! - dynamic segmentation with slow-start and time-based work stealing,
//! - a state file that only records progress already flushed to disk,
//! - `If-Range` validation on every resumed range request.

/// Engine version, reported by the CLI and the app.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
