//! Download engine for idm-alternative.
//!
//! - One preallocated (sparse) output file, written concurrently at offsets.
//! - Dynamic segmentation: the first connection starts at byte 0; further connections
//!   split the segment expected to finish last, sized by both connections' speeds, so a slow
//!   connection keeps only a sliver ([`segments`]).
//! - A state file that only records progress already flushed to disk.
//! - Every range response is checked (`If-Range`, `Content-Range`, size, HTML pages), and a
//!   server that ignores ranges falls back to a single connection.

mod download;
pub mod error;
mod file;
mod remote;
pub mod segments;
mod state;

pub use download::{Options, Outcome, Progress, Request, SegmentView, download};
pub use error::{Error, Result};
pub use tokio_util::sync::CancellationToken;

/// Engine version, reported by the CLI and the app.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
