//! Checking a finished download against a checksum the user supplied (e.g. from the
//! project's download page) or one found for it (see `discover`).

use std::fmt;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl Algorithm {
    fn digest_len(self) -> usize {
        match self {
            Algorithm::Md5 => 16,
            Algorithm::Sha1 => 20,
            Algorithm::Sha256 => 32,
            Algorithm::Sha512 => 64,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Algorithm::Md5 => "md5",
            Algorithm::Sha1 => "sha1",
            Algorithm::Sha256 => "sha256",
            Algorithm::Sha512 => "sha512",
        }
    }
}

/// An expected digest, written `algorithm:hex`, e.g. `sha256:9f86d0…`. A bare hex digest is
/// accepted too; its length tells the algorithm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checksum {
    pub algorithm: Algorithm,
    pub digest: Vec<u8>,
}

impl fmt::Display for Checksum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm.name(), hex(&self.digest))
    }
}

impl std::str::FromStr for Checksum {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let (algorithm, digest) = match s.split_once(':') {
            Some((name, digest)) => {
                let algorithm = match name.trim().to_ascii_lowercase().replace('-', "").as_str() {
                    "md5" => Algorithm::Md5,
                    "sha1" => Algorithm::Sha1,
                    "sha256" => Algorithm::Sha256,
                    "sha512" => Algorithm::Sha512,
                    other => {
                        return Err(format!(
                            "unknown checksum algorithm `{other}` (md5, sha1, sha256, sha512)"
                        ));
                    }
                };
                (algorithm, unhex(digest.trim())?)
            }
            None => {
                let digest = unhex(s)?;
                let algorithm = match digest.len() {
                    16 => Algorithm::Md5,
                    20 => Algorithm::Sha1,
                    32 => Algorithm::Sha256,
                    64 => Algorithm::Sha512,
                    n => return Err(format!("a {n}-byte digest matches no known algorithm")),
                };
                (algorithm, digest)
            }
        };
        if digest.len() != algorithm.digest_len() {
            return Err(format!(
                "a {} digest has {} hex digits, got {}",
                algorithm.name(),
                algorithm.digest_len() * 2,
                digest.len() * 2
            ));
        }
        Ok(Self { algorithm, digest })
    }
}

fn unhex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("not a hex digest: `{s}`"));
    }
    Ok((0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or_default())
        .collect())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) enum Hasher {
    Md5(Md5),
    Sha1(Sha1),
    Sha256(Sha256),
    Sha512(Sha512),
}

impl Hasher {
    pub(crate) fn new(a: Algorithm) -> Self {
        match a {
            Algorithm::Md5 => Hasher::Md5(Md5::new()),
            Algorithm::Sha1 => Hasher::Sha1(Sha1::new()),
            Algorithm::Sha256 => Hasher::Sha256(Sha256::new()),
            Algorithm::Sha512 => Hasher::Sha512(Sha512::new()),
        }
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::Md5(h) => h.update(data),
            Hasher::Sha1(h) => h.update(data),
            Hasher::Sha256(h) => h.update(data),
            Hasher::Sha512(h) => h.update(data),
        }
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        match self {
            Hasher::Md5(h) => h.finalize().to_vec(),
            Hasher::Sha1(h) => h.finalize().to_vec(),
            Hasher::Sha256(h) => h.finalize().to_vec(),
            Hasher::Sha512(h) => h.finalize().to_vec(),
        }
    }
}

/// Hashes `path` and compares it with `want`, counting the bytes hashed in `done`;
/// cancelling stops early with [`Error::Cancelled`].
pub(crate) async fn verify(
    path: &Path,
    want: &Checksum,
    cancel: &CancellationToken,
    done: Arc<AtomicU64>,
) -> Result<()> {
    let algorithm = want.algorithm;
    let got = digest(path, algorithm, cancel, done).await?;
    if got == want.digest {
        Ok(())
    } else {
        Err(Error::ChecksumMismatch {
            expected: want.to_string(),
            actual: format!("{}:{}", algorithm.name(), hex(&got)),
            origin: None,
        })
    }
}

/// The checksum of the file at `path` (e.g. to look it up, or to check it again).
pub async fn file_checksum(path: &Path, algorithm: Algorithm) -> Result<Checksum> {
    let digest = digest(path, algorithm, &CancellationToken::new(), Arc::default()).await?;
    Ok(Checksum { algorithm, digest })
}

async fn digest(
    path: &Path,
    algorithm: Algorithm,
    cancel: &CancellationToken,
    done: Arc<AtomicU64>,
) -> Result<Vec<u8>> {
    let path = path.to_owned();
    let cancel = cancel.clone();
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mut file = std::fs::File::open(&path)?;
        let mut hasher = Hasher::new(algorithm);
        let mut buf = vec![0u8; 1 << 20];
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let n = file.read(&mut buf)?;
            if n == 0 {
                return Ok(hasher.finish());
            }
            hasher.update(&buf[..n]);
            done.fetch_add(n as u64, Ordering::Relaxed);
        }
    })
    .await
    .map_err(std::io::Error::other)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        let c: Checksum =
            "SHA-256:9F86D081884C7D659A2FEAA0C55AD015A3BF4F1B2B0B822CD15D6C15B0F00A08"
                .parse()
                .expect("parse");
        assert_eq!(c.algorithm, Algorithm::Sha256);
        assert_eq!(
            c.to_string(),
            "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
        let bare: Checksum = "098f6bcd4621d373cade4e832627b4f6".parse().expect("md5");
        assert_eq!(bare.algorithm, Algorithm::Md5);
        assert!("sha256:abcd".parse::<Checksum>().is_err());
        assert!("crc32:abcd".parse::<Checksum>().is_err());
        assert!(
            "md5:zz8f6bcd4621d373cade4e832627b4f6"
                .parse::<Checksum>()
                .is_err()
        );
        assert!("abc".parse::<Checksum>().is_err());
    }

    #[tokio::test]
    async fn hashing() {
        let dir = std::env::temp_dir().join(format!("shardload-checksum-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let f = dir.join("t.txt");
        std::fs::write(&f, b"test").expect("write");
        let never = CancellationToken::new();
        for good in [
            "md5:098f6bcd4621d373cade4e832627b4f6",
            "sha1:a94a8fe5ccb19ba61c4c0873d391e987982fbbd3",
            "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "sha512:ee26b0dd4af7e749aa1a8ee3c10ae9923f618980772e473f8819a5d4940e0db27ac185f8a0e1d5f84f88bc887fd67b143732c304cc5fa9ad8e6f57f50028a8ff",
        ] {
            verify(&f, &good.parse().expect("parse"), &never, Arc::default())
                .await
                .unwrap_or_else(|e| panic!("{good}: {e}"));
        }
        let bad: Checksum = "md5:00000000000000000000000000000000"
            .parse()
            .expect("parse");
        match verify(&f, &bad, &never, Arc::default()).await {
            Err(Error::ChecksumMismatch { actual, .. }) => {
                assert_eq!(actual, "md5:098f6bcd4621d373cade4e832627b4f6");
            }
            other => panic!("expected mismatch, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
