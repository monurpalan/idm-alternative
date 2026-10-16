use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network: {0}")]
    Http(#[from] wreq::Error),
    #[error("disk: {0}")]
    Io(#[from] std::io::Error),
    #[error("server returned HTTP {0}")]
    Status(u16),
    /// The link stopped working (HTTP 401, 403 or 410), even after following the original
    /// URL's redirects again. Progress is kept: downloading from a fresh link of the same
    /// file resumes (see [`crate::patch_url`]).
    #[error(
        "the link has expired or access was denied (HTTP {0}); a fresh link resumes the download"
    )]
    LinkExpired(u16),
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
    /// Another download is writing the same file.
    #[error("{} is already being downloaded", .0.display())]
    InUse(std::path::PathBuf),
    #[error("cancelled")]
    Cancelled,
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
        )
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
