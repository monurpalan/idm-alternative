//! A download in the list, as stored and as shown.

use std::path::PathBuf;

use idma_engine::{Phase, SegmentView};
use serde::Serialize;

pub type TaskId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Waiting for a free download slot.
    Queued,
    Downloading,
    /// Stopped by the user; keeps its progress.
    Paused,
    Done,
    /// Stopped by an error (see [`Task::error`]); keeps its progress.
    Error,
}

impl Status {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Downloading => "downloading",
            Status::Paused => "paused",
            Status::Done => "done",
            Status::Error => "error",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "queued" => Status::Queued,
            "downloading" => Status::Downloading,
            "paused" => Status::Paused,
            "done" => Status::Done,
            _ => Status::Error,
        }
    }
}

/// A stream download: the task's URL is the video playlist (HLS); this says what goes with it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Stream {
    /// A separate audio playlist, joined with the video.
    pub audio: Option<String>,
    /// Downloaded by yt-dlp with this format selector; the task's URL is then the page.
    #[serde(default)]
    pub ytdlp: Option<String>,
    /// A DASH stream: the video representation id; the task's URL is the manifest and
    /// `audio` an audio representation id.
    #[serde(default)]
    pub dash: Option<String>,
    /// Subtitle tracks saved next to the video when it is done.
    #[serde(default)]
    pub subtitles: Vec<Subtitle>,
    /// A torrent (the task's URL is a magnet link or a `.torrent` file): which files and
    /// where they go.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent: Option<TorrentChoice>,
    /// Some files out of a ZIP archive (the task's URL is the archive): which, and where.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<ArchiveChoice>,
}

/// The files picked out of a ZIP archive on the server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct ArchiveChoice {
    /// Entry names (a folder brings what is in it; see [`idma_engine::select_zip_entries`]).
    pub entries: Vec<String>,
    /// The folder they go into, chosen on the first start.
    #[serde(default)]
    pub folder: Option<std::path::PathBuf>,
    /// The server turned out not to send parts of files: the whole archive is downloaded.
    #[serde(default)]
    pub whole: bool,
}

/// A torrent download's choices.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct TorrentChoice {
    /// Indices of the files to download (see [`crate::torrent::TorrentInfo::files`]); all
    /// when absent.
    #[serde(default)]
    pub files: Option<Vec<usize>>,
    /// The folder the torrent's files go into, chosen on the first start.
    #[serde(default)]
    pub folder: Option<std::path::PathBuf>,
}

/// A subtitle track of a stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Subtitle {
    /// HLS: the rendition's playlist; DASH: the text representation id.
    pub id: String,
    /// Language code, used in the file name (`film.tr.vtt`).
    pub language: Option<String>,
}

/// What a failure needs from the user; the UI offers the matching fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorKind {
    /// The server asks for a user name and password.
    Auth,
    /// The link stopped working: a fresh link continues the download.
    Expired,
    /// The file on the server changed (or a new link led to another file).
    Changed,
    /// Not enough disk space.
    Disk,
    /// The finished file does not match the given checksum.
    Checksum,
}

impl ErrorKind {
    pub(crate) fn of(e: &idma_engine::Error) -> Option<Self> {
        use idma_engine::Error as E;
        Some(match e {
            E::AuthRequired { .. } => ErrorKind::Auth,
            E::LinkExpired(_) => ErrorKind::Expired,
            E::FileChanged | E::HtmlResponse => ErrorKind::Changed,
            E::DiskFull { .. } => ErrorKind::Disk,
            E::ChecksumMismatch { .. } => ErrorKind::Checksum,
            _ => return None,
        })
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Auth => "auth",
            ErrorKind::Expired => "expired",
            ErrorKind::Changed => "changed",
            ErrorKind::Disk => "disk",
            ErrorKind::Checksum => "checksum",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "auth" => ErrorKind::Auth,
            "expired" => ErrorKind::Expired,
            "changed" => ErrorKind::Changed,
            "disk" => ErrorKind::Disk,
            "checksum" => ErrorKind::Checksum,
            _ => return None,
        })
    }
}

/// IDM's categories, picked from the file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    General,
    Compressed,
    Documents,
    Music,
    Programs,
    Video,
}

impl Category {
    pub fn for_name(name: &str) -> Self {
        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "zip" | "rar" | "7z" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "tar" | "arj" | "cab"
            | "iso" | "img" => Category::Compressed,
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "txt"
            | "rtf" | "epub" | "csv" => Category::Documents,
            "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "opus" | "wma" => Category::Music,
            "exe" | "msi" | "msix" | "appx" | "apk" | "dmg" | "pkg" | "deb" | "rpm" => {
                Category::Programs
            }
            "mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "flv" | "m4v" | "ts" | "mpg"
            | "mpeg" => Category::Video,
            _ => Category::General,
        }
    }

    pub const ALL: [Category; 6] = [
        Category::General,
        Category::Compressed,
        Category::Documents,
        Category::Music,
        Category::Programs,
        Category::Video,
    ];

    /// Folder name IDM uses for the category.
    pub fn folder(self) -> &'static str {
        match self {
            Category::General => "General",
            Category::Compressed => "Compressed",
            Category::Documents => "Documents",
            Category::Music => "Music",
            Category::Programs => "Programs",
            Category::Video => "Video",
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Category::General => "general",
            Category::Compressed => "compressed",
            Category::Documents => "documents",
            Category::Music => "music",
            Category::Programs => "programs",
            Category::Video => "video",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "compressed" => Category::Compressed,
            "documents" => Category::Documents,
            "music" => Category::Music,
            "programs" => Category::Programs,
            "video" => Category::Video,
            _ => Category::General,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: TaskId,
    pub url: String,
    /// Sent with every request, e.g. `Cookie`, `Referer` from the browser.
    pub headers: Vec<(String, String)>,
    pub dir: PathBuf,
    /// File name; `None` until the server was asked (or the user chose one).
    pub name: Option<String>,
    /// Where the file is written, once chosen.
    pub path: Option<PathBuf>,
    pub size: Option<u64>,
    /// Bytes on disk as of the last update (live progress comes as [`crate::Event::Progress`]).
    pub received: u64,
    pub status: Status,
    pub error: Option<String>,
    /// What kind of failure `error` is, when the user can do something about it.
    pub error_kind: Option<ErrorKind>,
    pub category: Category,
    pub connections: usize,
    /// This download's own speed limit, bytes per second.
    pub speed_limit: Option<u64>,
    pub checksum: Option<String>,
    /// Unix milliseconds.
    pub added_at: i64,
    pub finished_at: Option<i64>,
    /// Order in the queue: lower starts first.
    pub queue_pos: i64,
    /// Set for a stream (HLS) download.
    pub stream: Option<Stream>,
    /// Other addresses of the same file, used alongside `url`.
    pub mirrors: Vec<String>,
}

/// What to download; everything else has defaults from [`crate::Config`].
#[derive(Debug, Clone, Default)]
pub struct NewTask {
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// Folder to save into; `None` uses the configured one.
    pub dir: Option<PathBuf>,
    /// File name; `None` uses the server's suggestion.
    pub name: Option<String>,
    pub connections: Option<usize>,
    pub speed_limit: Option<u64>,
    /// `sha256:<hex>` and the like; checked when the download finishes.
    pub checksum: Option<String>,
    /// Queue it now; otherwise it is added paused.
    pub start: bool,
    /// A stream download (`url` is then the video playlist).
    pub stream: Option<Stream>,
    /// Other addresses of the same file (mirrors).
    pub mirrors: Vec<String>,
}

/// Live progress of a running download, sent about four times a second.
#[derive(Debug, Clone, Serialize)]
pub struct TaskProgress {
    pub id: TaskId,
    #[serde(serialize_with = "phase_name")]
    pub phase: Phase,
    pub size: Option<u64>,
    pub received: u64,
    /// Bytes per second.
    pub speed: f64,
    pub connections: usize,
    /// Bytes hashed so far while verifying a checksum.
    pub verified: u64,
    #[serde(serialize_with = "segments")]
    pub segments: Vec<SegmentView>,
}

fn phase_name<S: serde::Serializer>(p: &Phase, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(match p {
        Phase::Probing => "probing",
        Phase::Downloading => "downloading",
        Phase::Verifying => "verifying",
        Phase::Done => "done",
    })
}

/// `[start, end, received, active]` per segment, for the segment map.
fn segments<S: serde::Serializer>(v: &[SegmentView], s: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(v.len()))?;
    for seg in v {
        seq.serialize_element(&(seg.start, seg.end, seg.received, seg.active))?;
    }
    seq.end()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories() {
        assert_eq!(Category::for_name("a.ZIP"), Category::Compressed);
        assert_eq!(Category::for_name("src.tar.gz"), Category::Compressed);
        assert_eq!(Category::for_name("setup.exe"), Category::Programs);
        assert_eq!(Category::for_name("film.mkv"), Category::Video);
        assert_eq!(Category::for_name("song.flac"), Category::Music);
        assert_eq!(Category::for_name("paper.pdf"), Category::Documents);
        assert_eq!(Category::for_name("noext"), Category::General);
        for c in [
            Category::General,
            Category::Compressed,
            Category::Documents,
            Category::Music,
            Category::Programs,
            Category::Video,
        ] {
            assert_eq!(Category::parse(c.as_str()), c);
        }
    }
}
