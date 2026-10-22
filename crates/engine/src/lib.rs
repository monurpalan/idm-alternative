//! Download engine for Shardload.
//!
//! - One preallocated (sparse) output file, written concurrently at offsets.
//! - Dynamic segmentation: the first connection starts at byte 0; further connections
//!   split the segment expected to finish last, sized by both connections' speeds, so a slow
//!   connection keeps only a sliver ([`segments`]).
//! - Global and per-download speed limits that can change while downloads run ([`Limiter`]).
//! - A state file that only records progress already flushed to disk.
//! - Every range response is checked (`If-Range`, `Content-Range`, size, HTML pages), and a
//!   server that ignores ranges falls back to a single connection.
//! - Chosen files out of a ZIP archive on a server, without downloading the rest
//!   ([`RemoteZip`]).
//! - Without a checksum from the user, one published for the file (GitHub's digest of a
//!   release asset, `SHA256SUMS` and the like next to it) is looked for while it downloads
//!   and checked the same way.

mod checksum;
mod dash;
mod discover;
mod download;
pub mod error;
mod file;
mod hls;
mod limit;
pub mod metalink;
pub mod name;
mod pieces;
mod place;
mod relink;
mod remote;
pub mod segments;
mod state;
mod subtitles;
mod zip;
mod zone;

pub use checksum::{Algorithm, Checksum};
pub use dash::{DashRequest, download_dash};
pub use download::{
    Learned, Learning, Options, Outcome, Phase, Probe, Progress, Request, SegmentView, download,
    fetch_text, probe,
};
pub use error::{Error, Result};
pub use hls::{Clip, RecordRequest, StreamRequest, download_hls, record_hls};
pub use limit::Limiter;
pub use pieces::Pieces;
pub use place::{Conflict, target, target_avoiding};
pub use relink::patch_url;
pub use remote::{Browser, basic_auth};
pub use subtitles::{SubtitleSource, download_subtitles};
pub use tokio_util::sync::CancellationToken;
pub use zip::{
    Extracted, RemoteZip, ZipEntry, method_name, safe_path, select as select_zip_entries,
};
pub use zone::{mark_of_the_web, zone_identifier};

/// Engine version, reported by the CLI and the app.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
