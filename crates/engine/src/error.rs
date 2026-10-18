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
    #[error("server is busy (HTTP {0})")]
    Busy(u16),
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
    #[error("state file: {0}")]
    State(String),
    /// The finished file does not match the checksum the user gave, or one found for it
    /// (`origin` says where, e.g. `SHA256SUMS`). The file is kept (under its final name) so
    /// it can be inspected; downloading again starts over.
    #[error("checksum mismatch: expected {expected}, got {actual}{}", .origin.as_ref().map(|o| format!(" (checksum from {o})")).unwrap_or_default())]
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

impl Error {
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
                | Error::DiskFull { .. }
                | Error::NoRanges
                | Error::Archive(_)
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
