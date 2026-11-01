//! What a file really is, from its first bytes, and what that says about its name: a program
//! named like a document, a web page saved in place of the file, or a name that says nothing
//! (`download.php`, `file`) that its content can fix.

use std::io::Read;
use std::path::Path;

/// How much of the start of a file [`sniff`] looks at.
pub const HEAD: usize = 8 * 1024;

/// What a file's bytes say it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Runs when opened: a Windows, Linux or macOS program, an installer, a Java archive.
    Program,
    /// A script that names its interpreter (`#!`).
    Script,
    /// A Windows shortcut (`.lnk`), which can run anything.
    Shortcut,
    Archive,
    Document,
    Image,
    Audio,
    Video,
    /// A web page.
    Html,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Program => "program",
            Kind::Script => "script",
            Kind::Shortcut => "shortcut",
            Kind::Archive => "archive",
            Kind::Document => "document",
            Kind::Image => "image",
            Kind::Audio => "audio",
            Kind::Video => "video",
            Kind::Html => "html",
        }
    }

    /// Runs code when opened.
    pub fn runs(self) -> bool {
        matches!(self, Kind::Program | Kind::Script | Kind::Shortcut)
    }
}

/// A file's content type: its kind and the extension it usually has. `ext` is empty when the
/// bytes do not tell (an OLE file is a Word document as often as an installer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Content {
    pub kind: Kind,
    pub ext: &'static str,
}

impl Content {
    const fn of(kind: Kind, ext: &'static str) -> Self {
        Self { kind, ext }
    }

    /// `kind` or `kind:ext`, as stored.
    pub fn label(self) -> String {
        if self.ext.is_empty() {
            self.kind.as_str().to_owned()
        } else {
            format!("{}:{}", self.kind.as_str(), self.ext)
        }
    }

    /// The reverse of [`Content::label`].
    pub fn parse(label: &str) -> Option<Self> {
        let (kind, ext) = label.split_once(':').unwrap_or((label, ""));
        let kind = [
            Kind::Program,
            Kind::Script,
            Kind::Shortcut,
            Kind::Archive,
            Kind::Document,
            Kind::Image,
            Kind::Audio,
            Kind::Video,
            Kind::Html,
        ]
        .into_iter()
        .find(|k| k.as_str() == kind)?;
        let ext = EXTENSIONS
            .iter()
            .find(|e| **e == ext)
            .copied()
            .unwrap_or("");
        Some(Self { kind, ext })
    }
}

/// Every extension [`sniff`] can give (so a stored label maps back to a `'static` one).
const EXTENSIONS: &[&str] = &[
    "exe", "elf", "macho", "msi", "jar", "class", "lnk", "zip", "docx", "xlsx", "pptx", "odt",
    "ods", "odp", "epub", "apk", "7z", "rar", "gz", "xz", "bz2", "zst", "cab", "pdf", "png", "jpg",
    "gif", "webp", "tif", "mp3", "flac", "ogg", "wav", "mp4", "m4a", "mov", "mkv", "webm", "avi",
    "flv", "ts", "html", "sh",
];

/// The first [`HEAD`] bytes of the file at `path`.
pub fn read_head(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(HEAD);
    std::fs::File::open(path)?
        .take(HEAD as u64)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

/// What `head` (the start of a file) is, when its bytes say so for certain; `None` for text,
/// unknown formats and anything too short to tell.
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "one long sequence kept together; to be split when it next changes"
)]
pub fn sniff(head: &[u8]) -> Option<Content> {
    use Kind::*;
    let at = |offset: usize, magic: &[u8]| head.get(offset..offset + magic.len()) == Some(magic);
    if at(0, b"MZ") {
        // A text file can start with "MZ"; a program has the PE header where it says.
        let pe = head
            .get(0x3C..0x40)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .filter(|&off| at(off, b"PE\0\0"));
        return pe.map(|_| Content::of(Program, "exe"));
    }
    if at(0, b"\x7FELF") {
        return Some(Content::of(Program, "elf"));
    }
    for magic in [
        b"\xFE\xED\xFA\xCE",
        b"\xFE\xED\xFA\xCF",
        b"\xCE\xFA\xED\xFE",
        b"\xCF\xFA\xED\xFE",
    ] {
        if at(0, magic) {
            return Some(Content::of(Program, "macho"));
        }
    }
    if at(0, b"\xCA\xFE\xBA\xBE") {
        // A universal macOS program lists a few architectures; a Java class has its version
        // (45 or more) there.
        let n = u32::from_be_bytes(head.get(4..8)?.try_into().ok()?);
        return Some(if n < 20 {
            Content::of(Program, "macho")
        } else {
            Content::of(Program, "class")
        });
    }
    if at(0, b"#!") {
        let line = head[2..].split(|&b| b == b'\n').next().unwrap_or_default();
        let line = String::from_utf8_lossy(line);
        let interpreter = line.trim_start().starts_with('/')
            || line.trim_start().starts_with("usr/")
            || line.contains("env ");
        return interpreter.then_some(Content::of(Script, "sh"));
    }
    if at(0, b"\x4C\0\0\0\x01\x14\x02\0") {
        return Some(Content::of(Shortcut, "lnk"));
    }
    if at(0, b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1") {
        return Some(ole(head));
    }
    if at(0, b"PK\x03\x04") {
        return Some(zip_family(head));
    }
    if at(0, b"%PDF-") {
        return Some(Content::of(Document, "pdf"));
    }
    if at(0, b"7z\xBC\xAF\x27\x1C") {
        return Some(Content::of(Archive, "7z"));
    }
    if at(0, b"Rar!\x1A\x07") {
        return Some(Content::of(Archive, "rar"));
    }
    if at(0, b"\x1F\x8B\x08") {
        return Some(Content::of(Archive, "gz"));
    }
    if at(0, b"\xFD7zXZ\0") {
        return Some(Content::of(Archive, "xz"));
    }
    if at(0, b"BZh")
        && head.get(3).is_some_and(|b| (b'1'..=b'9').contains(b))
        && at(4, b"\x31\x41\x59\x26\x53\x59")
    {
        return Some(Content::of(Archive, "bz2"));
    }
    if at(0, b"\x28\xB5\x2F\xFD") {
        return Some(Content::of(Archive, "zst"));
    }
    if at(0, b"MSCF\0\0\0\0") {
        return Some(Content::of(Archive, "cab"));
    }
    if at(0, b"\x89PNG\r\n\x1A\n") {
        return Some(Content::of(Image, "png"));
    }
    if at(0, b"\xFF\xD8\xFF") {
        return Some(Content::of(Image, "jpg"));
    }
    if at(0, b"GIF87a") || at(0, b"GIF89a") {
        return Some(Content::of(Image, "gif"));
    }
    if at(0, b"II*\0") || at(0, b"MM\0*") {
        return Some(Content::of(Image, "tif"));
    }
    if at(0, b"RIFF") {
        if at(8, b"WEBP") {
            return Some(Content::of(Image, "webp"));
        }
        if at(8, b"WAVE") {
            return Some(Content::of(Audio, "wav"));
        }
        if at(8, b"AVI ") {
            return Some(Content::of(Video, "avi"));
        }
        return None;
    }
    if at(0, b"ID3") {
        return Some(Content::of(Audio, "mp3"));
    }
    if at(0, b"fLaC") {
        return Some(Content::of(Audio, "flac"));
    }
    if at(0, b"OggS") {
        return Some(Content::of(Audio, "ogg"));
    }
    if at(4, b"ftyp") {
        let brand = head.get(8..12)?;
        return Some(match brand {
            b"M4A " | b"M4B " => Content::of(Audio, "m4a"),
            b"qt  " => Content::of(Video, "mov"),
            _ => Content::of(Video, "mp4"),
        });
    }
    if at(0, b"\x1A\x45\xDF\xA3") {
        let webm = head.windows(4).take(64).any(|w| w == b"webm");
        return Some(Content::of(Video, if webm { "webm" } else { "mkv" }));
    }
    if at(0, b"FLV\x01") {
        return Some(Content::of(Video, "flv"));
    }
    // MPEG transport stream: a sync byte every 188 bytes.
    if head.len() >= 188 * 4 && (0..4).all(|i| head[i * 188] == 0x47) {
        return Some(Content::of(Video, "ts"));
    }
    is_html(head).then_some(Content::of(Html, "html"))
}

/// An OLE compound file: a Windows Installer package when its root says so, otherwise an
/// old Office document or something else of the kind.
fn ole(head: &[u8]) -> Content {
    // The root directory entry's class id; the directory starts at sector `first + 1`.
    const MSI: [u8; 16] = [
        0x84, 0x10, 0x0C, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x46,
    ];
    let shift = head
        .get(0x1E..0x20)
        .map_or(9, |b| u16::from_le_bytes([b[0], b[1]]));
    let first = head
        .get(0x30..0x34)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let clsid = first.and_then(|s| {
        let start = (s + 1).checked_mul(1 << shift.min(12))? + 0x50;
        head.get(start..start + 16)
    });
    if clsid == Some(&MSI[..]) {
        Content::of(Kind::Program, "msi")
    } else {
        Content::of(Kind::Document, "")
    }
}

/// A ZIP archive, or one of the formats built on ZIP, by the names of its first entries.
fn zip_family(head: &[u8]) -> Content {
    use Kind::*;
    let has = |needle: &[u8]| head.windows(needle.len()).any(|w| w == needle);
    if has(b"[Content_Types].xml") || has(b"_rels/.rels") {
        if has(b"word/") {
            return Content::of(Document, "docx");
        }
        if has(b"xl/") {
            return Content::of(Document, "xlsx");
        }
        if has(b"ppt/") {
            return Content::of(Document, "pptx");
        }
        return Content::of(Document, "");
    }
    // OpenDocument and EPUB start with an uncompressed `mimetype` entry.
    if head.get(30..38) == Some(b"mimetype") {
        let rest = head.get(38..120).unwrap_or_default();
        let has_rest = |needle: &[u8]| rest.windows(needle.len()).any(|w| w == needle);
        for (mime, ext) in [
            (&b"application/epub+zip"[..], "epub"),
            (b"opendocument.text", "odt"),
            (b"opendocument.spreadsheet", "ods"),
            (b"opendocument.presentation", "odp"),
        ] {
            if has_rest(mime) {
                return Content::of(Document, ext);
            }
        }
    }
    if has(b"AndroidManifest.xml") || has(b"classes.dex") {
        return Content::of(Archive, "apk");
    }
    if has(b"META-INF/MANIFEST.MF") {
        return Content::of(Program, "jar");
    }
    Content::of(Archive, "zip")
}

/// Starts like a web page, after any byte-order mark, white space and comments.
fn is_html(head: &[u8]) -> bool {
    let text = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    let mut s = String::from_utf8_lossy(&text[..text.len().min(1024)]).to_ascii_lowercase();
    loop {
        let t = s.trim_start();
        if let Some(rest) = t.strip_prefix("<!--") {
            match rest.find("-->") {
                Some(end) => s = rest[end + 3..].to_owned(),
                None => return false,
            }
        } else {
            s = t.to_owned();
            break;
        }
    }
    [
        "<!doctype html",
        "<html",
        "<head>",
        "<head ",
        "<body",
        "<title>",
    ]
    .iter()
    .any(|p| s.starts_with(p))
}

/// What the extension of a name says the file is; `None` for names that say nothing
/// (no extension, `.php`, `.bin`) or for text a script may look like.
pub fn kind_of_name(name: &str) -> Option<Kind> {
    use Kind::*;
    let (_, ext) = crate::name::split_extension(name);
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    Some(match ext.as_str() {
        "exe" | "msi" | "msix" | "appx" | "com" | "scr" | "pif" | "cpl" | "dll" | "sys" | "jar"
        | "app" | "dmg" | "pkg" | "deb" | "rpm" | "appimage" | "elf" | "run" => Program,
        "bat" | "cmd" | "ps1" | "vbs" | "vbe" | "js" | "jse" | "wsf" | "hta" | "sh" | "py" => {
            Script
        }
        "lnk" => Shortcut,
        "zip" | "rar" | "7z" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "tar" | "cab" | "iso"
        | "apk" => Archive,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp"
        | "rtf" | "epub" => Document,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "heic" | "svg" => Image,
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "opus" | "wma" => Audio,
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "flv" | "m4v" | "ts" | "mpg" | "mpeg" => {
            Video
        }
        "html" | "htm" | "xhtml" | "mht" => Html,
        _ => return None,
    })
}

/// A program named like a document or a video: `report.pdf.exe`. The name alone says so.
pub fn disguised_name(name: &str) -> bool {
    let (stem, ext) = crate::name::split_extension(name);
    let Some(Kind::Program | Kind::Script | Kind::Shortcut) = kind_of_name(name) else {
        return false;
    };
    // `.js` and `.py` are also what developers download on purpose; only the Windows ones.
    let ext = ext.to_ascii_lowercase();
    if matches!(
        ext.as_str(),
        ".py"
            | ".sh"
            | ".jar"
            | ".deb"
            | ".rpm"
            | ".dmg"
            | ".pkg"
            | ".app"
            | ".appimage"
            | ".run"
            | ".elf"
            | ".dll"
            | ".sys"
    ) {
        return false;
    }
    matches!(
        kind_of_name(stem),
        Some(Kind::Document | Kind::Image | Kind::Audio | Kind::Video | Kind::Archive)
    ) || stem.to_ascii_lowercase().ends_with(".txt")
}

/// Something worth telling about a finished file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finding {
    /// A program (or script, or shortcut) named like a document or media.
    Disguised,
    /// The server sent a web page where the name promised a file: often a sign-in page or a
    /// link that expired.
    WebPage,
}

impl Finding {
    pub fn as_str(self) -> &'static str {
        match self {
            Finding::Disguised => "disguised",
            Finding::WebPage => "webpage",
        }
    }
}

/// How a finished file's content and name disagree, if they do.
pub fn check(name: &str, content: Option<Content>) -> Option<Finding> {
    let named = kind_of_name(name);
    if let Some(c) = content {
        if c.kind.runs()
            && let Some(n) = named
            && !n.runs()
            && n != Kind::Html
            // A shebang script in a text-like name is just text to Windows.
            && !(c.kind == Kind::Script && n == Kind::Archive)
        {
            return Some(Finding::Disguised);
        }
        if c.kind == Kind::Html
            && let Some(n) = named
            && n != Kind::Html
            && n != Kind::Script
        {
            return Some(Finding::WebPage);
        }
    }
    disguised_name(name).then_some(Finding::Disguised)
}

/// Stems that say nothing about the file.
fn meaningless_stem(stem: &str) -> bool {
    let s = stem.trim().to_ascii_lowercase();
    s.is_empty()
        || s.chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == '_')
        || matches!(
            s.as_str(),
            "download"
                | "downloads"
                | "file"
                | "files"
                | "get"
                | "getfile"
                | "attachment"
                | "index"
                | "dl"
                | "fetch"
                | "view"
                | "open"
                | "content"
                | "data"
                | "blob"
                | "export"
                | "media"
                | "stream"
                | "default"
        )
}

/// Extensions of server scripts and placeholders: a name ending in one says nothing about
/// the file.
fn meaningless_ext(ext: &str) -> bool {
    matches!(
        ext.trim_start_matches('.').to_ascii_lowercase().as_str(),
        "" | "php"
            | "asp"
            | "aspx"
            | "jsp"
            | "cgi"
            | "pl"
            | "do"
            | "action"
            | "ashx"
            | "axd"
            | "cfm"
            | "dll"
            | "bin"
            | "dat"
            | "tmp"
            | "file"
            | "download"
    )
}

/// A better name for a file whose name says nothing (`download.php`, `file`, `12345`), from
/// what its content is and, when the stem says nothing either, the page's title. `None` when
/// the name is fine or the content does not tell.
pub fn better_name(name: &str, content: Option<Content>, title: Option<&str>) -> Option<String> {
    let c = content?;
    if c.ext.is_empty() || matches!(c.ext, "elf" | "macho" | "class") {
        return None;
    }
    let (stem, ext) = crate::name::split_extension(name);
    if !meaningless_ext(ext) && kind_of_name(name).is_some() {
        return None;
    }
    if !meaningless_ext(ext) && !ext.is_empty() {
        // An extension we do not know of (`.iso`, `.pak`): leave it.
        return None;
    }
    let stem = match title.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) if meaningless_stem(stem) => t.to_owned(),
        _ if stem.is_empty() => crate::name::FALLBACK.to_owned(),
        _ => stem.to_owned(),
    };
    let better = crate::name::sanitize(&format!("{stem}.{}", c.ext));
    (better != name).then_some(better)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pe() -> Vec<u8> {
        let mut b = vec![0u8; 512];
        b[..2].copy_from_slice(b"MZ");
        b[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        b
    }

    fn zip_with(first: &str) -> Vec<u8> {
        let mut b = b"PK\x03\x04".to_vec();
        b.resize(30, 0);
        b.extend_from_slice(first.as_bytes());
        b.extend_from_slice(&[0; 64]);
        b
    }

    fn kind(b: &[u8]) -> Option<(Kind, &'static str)> {
        sniff(b).map(|c| (c.kind, c.ext))
    }

    #[test]
    fn programs() {
        assert_eq!(kind(&pe()), Some((Kind::Program, "exe")));
        assert_eq!(kind(b"\x7FELF\x02\x01\x01\0"), Some((Kind::Program, "elf")));
        assert_eq!(
            kind(b"\xCF\xFA\xED\xFE\x07\0\0\x01"),
            Some((Kind::Program, "macho"))
        );
        assert_eq!(
            kind(b"\xCA\xFE\xBA\xBE\0\0\0\x02"),
            Some((Kind::Program, "macho"))
        );
        assert_eq!(
            kind(b"\xCA\xFE\xBA\xBE\0\0\0\x41"),
            Some((Kind::Program, "class"))
        );
        assert_eq!(kind(b"#!/bin/sh\necho hi\n"), Some((Kind::Script, "sh")));
        assert_eq!(
            kind(b"#!/usr/bin/env python3\n"),
            Some((Kind::Script, "sh"))
        );
        assert_eq!(
            kind(b"\x4C\0\0\0\x01\x14\x02\0\0\0\0\0\xC0\0\0\0"),
            Some((Kind::Shortcut, "lnk"))
        );
        assert_eq!(
            kind(&zip_with("META-INF/MANIFEST.MF")),
            Some((Kind::Program, "jar"))
        );
    }

    #[test]
    fn text_that_starts_like_a_program_is_not_one() {
        assert_eq!(
            kind(b"MZ is how DOS programs start. This file is text.\n"),
            None
        );
        let mut long = b"MZ notes ".to_vec();
        long.resize(600, b'x');
        assert_eq!(kind(&long), None);
        assert_eq!(kind(b"#! not a script, a heading\n"), None);
        assert_eq!(kind(b"plain text"), None);
        assert_eq!(kind(b""), None);
    }

    #[test]
    fn installers_and_office_files() {
        let mut msi = vec![0u8; 2048];
        msi[..8].copy_from_slice(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1");
        msi[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes());
        msi[0x30..0x34].copy_from_slice(&1u32.to_le_bytes());
        let root = 2 * 512 + 0x50;
        msi[root..root + 16].copy_from_slice(&[
            0x84, 0x10, 0x0C, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46,
        ]);
        assert_eq!(kind(&msi), Some((Kind::Program, "msi")));
        msi[root] = 0x06; // a Word document's class id
        assert_eq!(kind(&msi), Some((Kind::Document, "")));
    }

    #[test]
    fn zip_family() {
        let mut docx = zip_with("[Content_Types].xml");
        docx.extend_from_slice(b"PK\x03\x04word/document.xml");
        assert_eq!(kind(&docx), Some((Kind::Document, "docx")));
        assert_eq!(
            kind(&zip_with("AndroidManifest.xml")),
            Some((Kind::Archive, "apk"))
        );
        let mut epub = zip_with("mimetype");
        epub.splice(38..38, b"application/epub+zip".iter().copied());
        assert_eq!(kind(&epub), Some((Kind::Document, "epub")));
        let mut odt = zip_with("mimetype");
        odt.splice(
            38..38,
            b"application/vnd.oasis.opendocument.text".iter().copied(),
        );
        assert_eq!(kind(&odt), Some((Kind::Document, "odt")));
        assert_eq!(kind(&zip_with("readme.txt")), Some((Kind::Archive, "zip")));
    }

    #[test]
    fn media_and_documents() {
        assert_eq!(kind(b"%PDF-1.7\n"), Some((Kind::Document, "pdf")));
        assert_eq!(kind(b"\x89PNG\r\n\x1A\n\0\0"), Some((Kind::Image, "png")));
        assert_eq!(
            kind(b"\xFF\xD8\xFF\xE0\0\x10JFIF"),
            Some((Kind::Image, "jpg"))
        );
        assert_eq!(kind(b"GIF89a.."), Some((Kind::Image, "gif")));
        assert_eq!(kind(b"RIFF\0\0\0\0WEBPVP8 "), Some((Kind::Image, "webp")));
        assert_eq!(kind(b"RIFF\0\0\0\0WAVEfmt "), Some((Kind::Audio, "wav")));
        assert_eq!(kind(b"RIFF\0\0\0\0AVI LIST"), Some((Kind::Video, "avi")));
        assert_eq!(kind(b"ID3\x04\0\0"), Some((Kind::Audio, "mp3")));
        assert_eq!(kind(b"fLaC\0\0"), Some((Kind::Audio, "flac")));
        assert_eq!(kind(b"\0\0\0\x20ftypisom\0\0"), Some((Kind::Video, "mp4")));
        assert_eq!(kind(b"\0\0\0\x20ftypM4A \0\0"), Some((Kind::Audio, "m4a")));
        assert_eq!(kind(b"\0\0\0\x14ftypqt  \0\0"), Some((Kind::Video, "mov")));
        assert_eq!(
            kind(b"\x1A\x45\xDF\xA3\x9F\x42\x86\x81\x01\x42\x82\x84webm"),
            Some((Kind::Video, "webm"))
        );
        assert_eq!(
            kind(b"\x1A\x45\xDF\xA3\x9F\x42\x82\x88matroska"),
            Some((Kind::Video, "mkv"))
        );
        let mut ts = vec![0u8; 188 * 4];
        for i in 0..4 {
            ts[i * 188] = 0x47;
        }
        assert_eq!(kind(&ts), Some((Kind::Video, "ts")));
        assert_eq!(
            kind(b"7z\xBC\xAF\x27\x1C\0\x04"),
            Some((Kind::Archive, "7z"))
        );
        assert_eq!(kind(b"Rar!\x1A\x07\x01\0"), Some((Kind::Archive, "rar")));
        assert_eq!(kind(b"\x1F\x8B\x08\0"), Some((Kind::Archive, "gz")));
        assert_eq!(kind(b"BZh91AY&SY"), Some((Kind::Archive, "bz2")));
        assert_eq!(kind(b"BZh is not bzip2"), None);
    }

    #[test]
    fn web_pages() {
        assert_eq!(kind(b"<!DOCTYPE html><html>"), Some((Kind::Html, "html")));
        assert_eq!(
            kind(b"\xEF\xBB\xBF\n  <html lang=en>"),
            Some((Kind::Html, "html"))
        );
        assert_eq!(
            kind(b"<!-- served by nginx -->\n<HTML><HEAD>"),
            Some((Kind::Html, "html"))
        );
        assert_eq!(kind(b"<?xml version=\"1.0\"?><svg>"), None);
        assert_eq!(kind(b"Use <html> tags like this"), None);
    }

    #[test]
    fn labels_round_trip() {
        for c in [
            Content::of(Kind::Program, "exe"),
            Content::of(Kind::Document, ""),
            Content::of(Kind::Html, "html"),
        ] {
            assert_eq!(Content::parse(&c.label()), Some(c));
        }
        assert_eq!(Content::parse("nonsense"), None);
    }

    #[test]
    fn findings() {
        let exe = sniff(&pe());
        let html = sniff(b"<!doctype html><title>Sign in</title>");
        let zip = sniff(&zip_with("a.txt"));
        let sh = sniff(b"#!/bin/sh\n");
        assert_eq!(check("invoice.pdf", exe), Some(Finding::Disguised));
        assert_eq!(check("holiday.mp4", exe), Some(Finding::Disguised));
        assert_eq!(check("setup.exe", exe), None);
        assert_eq!(check("tool", exe), None);
        assert_eq!(check("download.php", exe), None);
        assert_eq!(check("photo.jpg", sh), Some(Finding::Disguised));
        assert_eq!(check("notes.txt", sh), None);
        assert_eq!(check("report.docx", zip), None, "a .docx is a ZIP");
        assert_eq!(check("app.apk", zip), None, "an .apk is a ZIP");
        assert_eq!(check("data.zip", html), Some(Finding::WebPage));
        assert_eq!(check("setup.exe", html), Some(Finding::WebPage));
        assert_eq!(check("page.html", html), None);
        assert_eq!(check("download.php", html), None);
        assert_eq!(check("download", html), None);
        // The name alone, with no content check (a file not read, an old download).
        assert_eq!(check("report.pdf.exe", None), Some(Finding::Disguised));
        assert_eq!(check("clip.mp4.scr", exe), Some(Finding::Disguised));
        assert_eq!(check("notes.txt.lnk", None), Some(Finding::Disguised));
        assert_eq!(check("tool-1.2.exe", None), None);
        assert_eq!(check("script.min.js", None), None);
        assert_eq!(check("archive.tar.gz", None), None);
        assert_eq!(check("lib.py", None), None);
    }

    #[test]
    fn better_names() {
        let pdf = sniff(b"%PDF-1.4");
        let exe = sniff(&pe());
        assert_eq!(
            better_name("download.php", pdf, None).as_deref(),
            Some("download.pdf")
        );
        assert_eq!(
            better_name("download.php", pdf, Some("Annual report 2025")).as_deref(),
            Some("Annual report 2025.pdf")
        );
        assert_eq!(better_name("file", exe, None).as_deref(), Some("file.exe"));
        assert_eq!(
            better_name("12345", pdf, Some(" Q3 ")).as_deref(),
            Some("Q3.pdf")
        );
        assert_eq!(
            better_name("thesis", pdf, Some("Site")).as_deref(),
            Some("thesis.pdf")
        );
        assert_eq!(better_name("report.pdf", pdf, None), None);
        // The name says what it is, even if the content disagrees: a warning, not a rename.
        assert_eq!(better_name("invoice.pdf", exe, None), None);
        assert_eq!(
            better_name("game.pak", pdf, None),
            None,
            "an unknown extension is kept"
        );
        assert_eq!(better_name("download.php", None, None), None);
        assert_eq!(
            better_name("download.php", sniff(b"\x7FELF\x02"), None),
            None
        );
        assert_eq!(
            better_name(
                "get.php",
                sniff(&{
                    let mut d = zip_with("[Content_Types].xml");
                    d.extend_from_slice(b"xl/workbook.xml");
                    d
                }),
                Some("Prices: 2025?")
            )
            .as_deref(),
            Some("Prices_ 2025_.xlsx")
        );
    }
}
