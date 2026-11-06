use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network: {0}")]
    Http(#[from] wreq::Error),
    #[error("disk: {0}")]
    Io(std::io::Error),
    /// Not enough room for the rest of the file (`need`/`free` known when checked before
    /// writing), or a write failed because the disk filled up. Progress is kept: the
    /// download resumes once space is freed.
    #[error("{}", disk_full_message(*.need, *.free))]
    DiskFull {
        need: Option<u64>,
        free: Option<u64>,
    },
    #[error("server returned HTTP {0}")]
    Status(u16),
    /// The link stopped working (HTTP 401, 403 or 410), even after following the original
    /// URL's redirects again. Progress is kept: downloading from a fresh link of the same
    /// file resumes (see [`crate::patch_url`]).
    #[error(
        "the link has expired or access was denied (HTTP {0}); a fresh link resumes the download"
    )]
    LinkExpired(u16),
    /// The server asks for a user name and password (HTTP 401 with `WWW-Authenticate`).
    /// Sending them (e.g. [`crate::basic_auth`] as an `Authorization` header) continues the
    /// download.
    #[error("the server asks for a user name and password{}", .realm.as_ref().map(|r| format!(" ({r})")).unwrap_or_default())]
    AuthRequired { realm: Option<String> },
    /// SFTP: the user's SSH key `key` is protected by a passphrase, and none was given or
    /// (`wrong`) the one given does not open it. The download's password is tried as the
    /// passphrase.
    #[error(
        "the SSH key {key} is protected by a passphrase: {}",
        if *.wrong { "the password given is not it" } else { "give it as the password" }
    )]
    KeyLocked { key: String, wrong: bool },
    /// The server is busy (HTTP 429 or 503), and may say for how long (`Retry-After`, given
    /// in seconds); that connection waits at least that long before trying again.
    #[error("server is busy (HTTP {0})")]
    Busy(u16, Option<Duration>),
    #[error("the file on the server changed since the download started")]
    FileChanged,
    #[error("server sent an HTML page instead of the file")]
    HtmlResponse,
    #[error("server ignored the range request")]
    RangeIgnored,
    #[error("bad range response: {0}")]
    BadRange(String),
    #[error("connection ended early")]
    Truncated,
    #[error("no data received for {0:?}")]
    Stalled(Duration),
    #[error("gave up after {attempts} failed attempts; last error: {last}")]
    TooManyFailures { attempts: u32, last: String },
    /// An FTP or SSH server, or the connection to it, failed (see [`crate::protocol`]): worth
    /// trying again, like a network error.
    #[error("{0}")]
    Remote(String),
    /// An FTP or SFTP server has no such file.
    #[error("the server has no file at {0}")]
    NotFound(String),
    /// Something the download cannot go on with, said as it is: a playlist or a manifest it
    /// cannot use, a tool that is missing. (It once said "state file:" before every one.)
    #[error("{0}")]
    State(String),
    /// The finished file does not match the checksum the user gave, or one found for it
    /// (`origin` says where, e.g. `SHA256SUMS`). The file is kept (under its final name) so
    /// it can be inspected; downloading again starts over.
    #[error("{}", mismatch(expected, actual, origin.as_deref()))]
    ChecksumMismatch {
        expected: String,
        actual: String,
        origin: Option<String>,
    },
    /// Another download is writing the same file.
    #[error("{} is already being downloaded", .0.display())]
    InUse(std::path::PathBuf),
    #[error("cancelled")]
    Cancelled,
    /// Reading a ZIP archive's list of files needs range requests, which this server does
    /// not answer: only downloading the whole file works.
    #[error(
        "the server does not support range requests, so files cannot be picked out of the archive; download the whole archive instead"
    )]
    NoRanges,
    /// Not a ZIP archive, or one this reader cannot handle.
    #[error("not a readable ZIP archive: {0}")]
    Archive(String),
}

/// A checksum only shown on a web page is a weaker promise than a publisher's checksum file:
/// a mismatch says the file differs from what the page shows, not that it is corrupt.
fn mismatch(expected: &str, actual: &str, origin: Option<&str>) -> String {
    match origin {
        Some(crate::checksum::FROM_PAGE) => format!(
            "the file does not match the checksum shown on the page it was linked from \
             (expected {expected}, got {actual}): the page may be out of date, or the file \
             not the one it describes"
        ),
        Some(o) => {
            format!("checksum mismatch: expected {expected}, got {actual} (checksum from {o})")
        }
        None => format!("checksum mismatch: expected {expected}, got {actual}"),
    }
}

impl Error {
    /// The server took the connection and cut it before answering, which some do at a TLS
    /// fingerprint they do not like. Not when it was never reached (refused, unreachable, no
    /// such host, too slow, the proxy): those fail the same way whatever identity is shown.
    pub fn cut_off(&self) -> bool {
        use std::io::ErrorKind as K;
        let Error::Http(e) = self else {
            return false;
        };
        if e.is_timeout() || e.is_dns() || e.is_proxy_connect() {
            return false;
        }
        let mut source = std::error::Error::source(e);
        while let Some(err) = source {
            if let Some(io) = err.downcast_ref::<std::io::Error>()
                && matches!(
                    io.kind(),
                    K::ConnectionRefused
                        | K::HostUnreachable
                        | K::NetworkUnreachable
                        | K::AddrNotAvailable
                        | K::TimedOut
                )
            {
                return false;
            }
            source = err.source();
        }
        true
    }

    /// Errors after which retrying the same request cannot help.
    pub(crate) fn is_fatal(&self) -> bool {
        matches!(
            self,
            Error::FileChanged
                | Error::HtmlResponse
                | Error::Io(_)
                | Error::Cancelled
                | Error::LinkExpired(_)
                | Error::AuthRequired { .. }
                | Error::KeyLocked { .. }
                | Error::DiskFull { .. }
                | Error::NoRanges
                | Error::Archive(_)
                | Error::NotFound(_)
        )
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        // ERROR_HANDLE_DISK_FULL (39) and ERROR_DISK_FULL (112) on Windows.
        if e.kind() == std::io::ErrorKind::StorageFull
            || (cfg!(windows) && matches!(e.raw_os_error(), Some(39 | 112)))
        {
            Error::DiskFull {
                need: None,
                free: None,
            }
        } else {
            Error::Io(e)
        }
    }
}

fn disk_full_message(need: Option<u64>, free: Option<u64>) -> String {
    match (need, free) {
        (Some(need), Some(free)) => format!(
            "not enough disk space: {} MiB more needed, {} MiB free",
            need.div_ceil(1 << 20),
            free >> 20
        ),
        _ => "the disk is full".to_owned(),
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_disk_writes_become_disk_full() {
        let full = Error::from(std::io::Error::from(std::io::ErrorKind::StorageFull));
        assert!(matches!(full, Error::DiskFull { need: None, .. }));
        assert_eq!(full.to_string(), "the disk is full");
        #[cfg(windows)]
        assert!(matches!(
            Error::from(std::io::Error::from_raw_os_error(112)),
            Error::DiskFull { .. }
        ));
        assert!(matches!(
            Error::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            Error::Io(_)
        ));
        let checked = Error::DiskFull {
            need: Some(3 << 20),
            free: Some(1 << 20),
        };
        assert_eq!(
            checked.to_string(),
            "not enough disk space: 3 MiB more needed, 1 MiB free"
        );
    }
}
