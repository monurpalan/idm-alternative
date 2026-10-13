//! Hostile HTTP test server for idm-alternative.
//!
//! `GET /file/{name}?<params>` serves a deterministic file whose behaviour is chosen by
//! query parameters (see [`scenario::Scenario`]): slow or stalling connections, dropped
//! connections, missing range support, expiring links, changing validators, HTML error
//! pages, connection limits, redirects and more.
//!
//! `GET /_stats` reports per-URL request counts, peak concurrency and bytes sent;
//! `POST /_reset` clears them.

pub mod content;
pub mod scenario;
mod server;

pub use server::router;

use std::io::Read;
use std::path::Path;

/// Outcome of comparing a downloaded file with the expected content.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    WrongLength { expected: u64, actual: u64 },
    Mismatch { offset: u64 },
}

/// Checks that `path` holds exactly `size` bytes of the content generated from `seed`.
pub fn verify_file(path: &Path, seed: u64, size: u64) -> std::io::Result<Verdict> {
    let actual = std::fs::metadata(path)?.len();
    if actual != size {
        return Ok(Verdict::WrongLength {
            expected: size,
            actual,
        });
    }
    let mut file = std::io::BufReader::with_capacity(4 << 20, std::fs::File::open(path)?);
    let mut buf = vec![0u8; 4 << 20];
    let mut offset = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(Verdict::Ok);
        }
        if let Some(bad) = content::first_mismatch(seed, offset, &buf[..n]) {
            return Ok(Verdict::Mismatch { offset: bad });
        }
        offset += n as u64;
    }
}
