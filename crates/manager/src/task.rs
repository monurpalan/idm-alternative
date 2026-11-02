//! A download in the list, as stored and as shown.

use std::path::PathBuf;

use serde::Serialize;
use shardload_engine::{Phase, SegmentView};

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
#[derive(Debug, Clone, Default, PartialEq, Serialize, serde::Deserialize)]
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
    /// Only this part of the video (a page or a stream); needs ffmpeg.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<crate::clip::Clip>,
    /// A live stream, recorded until it ends, the user stops it, or the limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<Recording>,
}

/// How a live stream is recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Recording {
    /// Stop after this many seconds.
    #[serde(default)]
    pub limit: Option<u64>,
    /// A page of a site yt-dlp knows: from the start of the broadcast rather than from now
    /// (`--live-from-start`, where the site keeps it).
    #[serde(default)]
    pub from_start: bool,
    /// Start at this time (Unix milliseconds) rather than when its turn comes.
    #[serde(default)]
    pub start_at: Option<i64>,
}

/// The files picked out of a ZIP archive on the server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct ArchiveChoice {
    /// Entry names (a folder brings what is in it; see [`shardload_engine::select_zip_entries`]).
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
    /// No route to the internet: the download is queued and checks again on its own. Not a
    /// failure, so nothing needs doing.
    Offline,
}

impl ErrorKind {
    pub(crate) fn of(e: &shardload_engine::Error) -> Option<Self> {
        use shardload_engine::Error as E;
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
            ErrorKind::Offline => "offline",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "auth" => ErrorKind::Auth,
            "expired" => ErrorKind::Expired,
            "changed" => ErrorKind::Changed,
            "disk" => ErrorKind::Disk,
            "checksum" => ErrorKind::Checksum,
            "offline" => ErrorKind::Offline,
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
    /// Given by the user, or found for the download (see `checksum_from`).
    pub checksum: Option<String>,
    /// Where `checksum` was found when the user gave none, e.g. `SHA256SUMS` or
    /// `GitHub release`.
    pub checksum_from: Option<String>,
    /// Unix milliseconds.
    pub added_at: i64,
    pub finished_at: Option<i64>,
    /// Order in the queue: lower starts first.
    pub queue_pos: i64,
    /// Set for a stream (HLS) download.
    pub stream: Option<Stream>,
    /// Other addresses of the same file, used alongside `url`.
    pub mirrors: Vec<String>,
    /// Where `url` led after redirects, learned on the first start.
    pub final_url: Option<String>,
    /// Added together with others (see [`Group`]).
    pub group_id: Option<i64>,
    /// Hashes of the file's pieces (from a metalink): a corrupt piece is downloaded again
    /// alone. Not sent to the UI.
    #[serde(skip)]
    pub pieces: Option<shardload_engine::Pieces>,
    /// When `url` was given (Unix ms): `added_at`, or when the link was last refreshed.
    #[serde(skip)]
    pub link_at: i64,
    /// What the finished file's first bytes say it is, e.g. `program:exe` (see
    /// [`shardload_engine::sniff::Content::label`]).
    pub content: Option<String>,
    /// Its name and its content disagree, or its name alone looks like a trick.
    pub warning: Option<Warning>,
    /// The file name was given (by the user or the browser) rather than taken from the
    /// server: it is never changed for a better one.
    #[serde(skip)]
    pub name_given: bool,
    /// The title of the page the download came from.
    #[serde(skip)]
    pub page_title: Option<String>,
    /// A checksum shown on that page (`algorithm:hex`), checked when the publisher posts
    /// none; once used it is `checksum`, with `checksum_from` [`shardload_engine::FROM_PAGE`].
    #[serde(skip)]
    pub page_checksum: Option<String>,
    /// What it asked to have done once finished (see [`crate::after`]).
    pub after: Option<crate::after::After>,
    /// How that, and the rules that took it, went.
    pub after_log: Vec<crate::after::Done>,
    /// Without a checksum: how it checked out by its own structure.
    pub structure: Option<Structure>,
    /// Wanted finished by this time, Unix milliseconds (see [`crate::deadline`]).
    pub deadline: Option<i64>,
}

/// A finished file checked by its own structure (see [`shardload_engine::structure`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Structure {
    /// `ZIP`, `gzip`, `xz`, `PNG`, `PDF`, `MP4`, `Matroska`.
    pub format: String,
    /// Bytes that were damaged and downloaded again.
    pub repaired: u64,
    /// Still damaged after that: the file on the server is.
    pub damaged: bool,
}

impl From<&shardload_engine::structure::Checked> for Structure {
    fn from(c: &shardload_engine::structure::Checked) -> Self {
        Self {
            format: c.format.name().to_owned(),
            repaired: c.repaired,
            damaged: c.damaged,
        }
    }
}

/// A finished file that may not be what it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Warning {
    /// `disguised`: a program named like a document or media; `webpage`: a web page saved
    /// where the name promised a file.
    #[serde(serialize_with = "finding_name")]
    pub kind: shardload_engine::sniff::Finding,
    /// `content` when the file's bytes said so, `name` when only its name does.
    pub by: &'static str,
}

impl Warning {
    /// What `name` and the stored content label say together.
    pub(crate) fn of(name: &str, content: Option<&str>) -> Option<Self> {
        use shardload_engine::sniff;
        let content = content.and_then(sniff::Content::parse);
        let kind = sniff::check(name, content)?;
        let by = if sniff::check(name, None) == Some(kind) {
            "name"
        } else {
            "content"
        };
        Some(Self { kind, by })
    }
}

fn finding_name<S: serde::Serializer>(
    f: &shardload_engine::sniff::Finding,
    s: S,
) -> Result<S::Ok, S::Error> {
    s.serialize_str(f.as_str())
}

/// Downloads added together: a playlist, a channel, a batch, the files of a site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Group {
    pub id: i64,
    pub name: String,
    /// Unix milliseconds.
    pub added_at: i64,
}

/// A finished download as the history keeps it, also after it left the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryEntry {
    pub id: i64,
    /// The download, while it is on the list.
    pub task_id: Option<TaskId>,
    pub url: String,
    pub final_url: Option<String>,
    /// The page it was downloaded from (the referrer), if known.
    pub page: Option<String>,
    pub name: String,
    pub path: PathBuf,
    pub size: Option<u64>,
    /// `algorithm:hex` the file was checked against.
    pub checksum: Option<String>,
    /// `sha256:<hex>` of the file, once known (see [`crate::dedup`]).
    pub sha256: Option<String>,
    /// Unix milliseconds.
    pub finished_at: i64,
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
    /// One of a group added together (see [`crate::Manager::new_group`]).
    pub group: Option<i64>,
    /// Hashes of the file's pieces (a metalink's are taken by themselves).
    pub pieces: Option<shardload_engine::Pieces>,
    /// A `.torrent` of the same file (a link or a path on disk): its piece hashes check the
    /// download, so a corrupt piece is downloaded again alone. Single-file torrents only.
    pub pieces_from: Option<String>,
    /// The title of the page the link was on: names a file whose name says nothing
    /// (`download.php`).
    pub title: Option<String>,
    /// A checksum shown next to the link on that page (`sha256:<hex>`…): a weaker promise than
    /// `checksum` or a publisher's checksum file, so it is checked only when neither exists.
    /// One that does not parse is left out.
    pub page_checksum: Option<String>,
    /// The size the server (or the browser, or a torrent's file list) said when asked before
    /// adding: what the queue's disk check counts until the download starts and knows.
    pub size: Option<u64>,
    /// Unpack it or move it once finished (see [`crate::after`]).
    pub after: Option<crate::after::After>,
    /// Wanted finished by this time, Unix milliseconds.
    pub deadline: Option<i64>,
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
    /// While verifying: where the checksum was found, when the user gave none.
    pub checksum_from: Option<String>,
    /// While recording a live stream: seconds recorded.
    pub recorded: f64,
    /// Seconds left, `[soonest, latest]` (see [`crate::eta`]); `None` while calculating.
    pub eta: Option<[f64; 2]>,
    /// What holds the download back, when it is known for certain (see [`crate::hints`]).
    pub hints: Vec<crate::hints::Hint>,
    #[serde(serialize_with = "segments")]
    pub segments: Vec<SegmentView>,
}

fn phase_name<S: serde::Serializer>(p: &Phase, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(match p {
        Phase::Probing => "probing",
        Phase::SettingUp => "settingup",
        Phase::Downloading => "downloading",
        Phase::Verifying => "verifying",
        Phase::Recording => "recording",
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
pub(crate) mod tests {
    use super::*;

    /// A finished download of `url` named `name`, for tests of what happens to it.
    pub(crate) fn sample(url: &str, name: &str) -> Task {
        Task {
            id: 1,
            url: url.into(),
            headers: Vec::new(),
            dir: PathBuf::from("."),
            name: Some(name.into()),
            path: Some(PathBuf::from(name)),
            size: Some(1),
            received: 1,
            status: Status::Done,
            error: None,
            error_kind: None,
            category: Category::for_name(name),
            connections: 1,
            speed_limit: None,
            checksum: None,
            checksum_from: None,
            added_at: 0,
            finished_at: Some(0),
            queue_pos: 0,
            stream: None,
            mirrors: Vec::new(),
            final_url: None,
            group_id: None,
            pieces: None,
            link_at: 0,
            content: None,
            warning: None,
            name_given: false,
            page_title: None,
            page_checksum: None,
            after: None,
            after_log: Vec::new(),
            structure: None,
            deadline: None,
        }
    }

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
