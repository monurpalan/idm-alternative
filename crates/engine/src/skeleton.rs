//! The skeleton first: what an archive, a video or a document holds, read from the few parts
//! of it that say so while the rest downloads — a ZIP's list of files at its end, an MP4's
//! `moov` box (at its start or its end) for the length and the picture size, a PDF's page
//! count. A few small range requests of their own, apart from the download's connections.

use serde::{Deserialize, Serialize};

use crate::download::{Options, http_config};
use crate::error::{Error, Result};
use crate::remote::{Http, Remote};

/// Bytes read from the start of an MP4 first: its first boxes' headers (a `moov` larger
/// than the rest is then read whole on its own).
const VIDEO_HEAD: u64 = 64 << 10;
/// Bytes read from the start and the end of a PDF: a linearized one says its page count in
/// its first kilobyte; the page tree's root is usually near one end.
const PDF_HEAD: u64 = 16 << 10;
const PDF_TAIL: u64 = 64 << 10;
/// A `moov` larger than this is not read (a very long video's index can be tens of MB).
const MAX_MOOV: u64 = 32 << 20;
/// Top-level boxes walked before giving up.
const MAX_BOXES: usize = 32;
/// Names of an archive's files kept for showing.
const NAMES: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Skeleton {
    Archive {
        files: u64,
        /// Bytes once unpacked.
        unpacked: u64,
        /// The first files' names.
        names: Vec<String>,
    },
    Video {
        /// Seconds.
        duration: f64,
        width: u32,
        height: u32,
    },
    Document {
        pages: u32,
    },
}

/// The skeleton of the file at `url` named `name`, when it is a kind that has one and the
/// server sends parts of files.
pub async fn skeleton(
    url: &str,
    headers: &[(String, String)],
    opts: &Options,
    name: &str,
) -> Result<Option<Skeleton>> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("zip") => archive(url, headers, opts).await,
        Some("mp4" | "m4v" | "mov" | "m4a") => {
            let http = Http::new(&http_config(headers, opts))?;
            video(&http, url).await
        }
        Some("pdf") => {
            let http = Http::new(&http_config(headers, opts))?;
            document(&http, url).await
        }
        _ => Ok(None),
    }
}

async fn archive(
    url: &str,
    headers: &[(String, String)],
    opts: &Options,
) -> Result<Option<Skeleton>> {
    let zip = match crate::zip::RemoteZip::open(url, headers, opts).await {
        Ok(z) => z,
        Err(Error::NoRanges | Error::Archive(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    let files: Vec<_> = zip.entries().iter().filter(|e| !e.is_dir).collect();
    Ok(Some(Skeleton::Archive {
        files: files.len() as u64,
        unpacked: files.iter().map(|e| e.size).sum(),
        names: files.iter().take(NAMES).map(|e| e.name.clone()).collect(),
    }))
}

/// Finds `moov` by walking the top-level boxes from the start, reading only their headers
/// past the first bytes, and reads it.
async fn video(http: &Http, url: &str) -> Result<Option<Skeleton>> {
    let (remote, head) = http.probe_sample(url, VIDEO_HEAD, 1 << 40).await?;
    let (Some(size), Some(head)) = (remote.size, head) else {
        return Ok(None);
    };
    let mut at = 0u64;
    for _ in 0..MAX_BOXES {
        if at + 8 > size {
            break;
        }
        let header = bytes(http, &remote, &head, at, 16.min(size - at)).await?;
        let Some((kind, len)) = box_header(&header, size - at) else {
            break;
        };
        if &kind == b"moov" {
            if len > MAX_MOOV {
                return Ok(None);
            }
            let moov = bytes(http, &remote, &head, at, len).await?;
            return Ok(parse_moov(&moov));
        }
        at += len;
    }
    Ok(None)
}

/// `len` bytes at `at`: from `head` when they are in it, otherwise fetched.
async fn bytes(http: &Http, remote: &Remote, head: &[u8], at: u64, len: u64) -> Result<Vec<u8>> {
    if at + len <= head.len() as u64 {
        return Ok(head[at as usize..(at + len) as usize].to_vec());
    }
    http.fetch(remote, at, at + len, 1 << 40).await
}

/// A box's type and whole length, when `b` starts with a box header that fits in `left`.
fn box_header(b: &[u8], left: u64) -> Option<([u8; 4], u64)> {
    let short = u64::from(u32::from_be_bytes(b.get(0..4)?.try_into().ok()?));
    let kind: [u8; 4] = b.get(4..8)?.try_into().ok()?;
    let len = match short {
        0 => left,
        1 => u64::from_be_bytes(b.get(8..16)?.try_into().ok()?),
        n => n,
    };
    (len >= 8 && len <= left && kind.iter().all(|c| c.is_ascii_graphic() || *c == b' '))
        .then_some((kind, len))
}

/// The children of a box's body.
fn children(body: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 8 <= body.len() {
        let Some((kind, len)) = box_header(&body[at..], (body.len() - at) as u64) else {
            break;
        };
        let header =
            if u32::from_be_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]) == 1 {
                16
            } else {
                8
            };
        // A 64-bit length shorter than its own header says nothing.
        let Some(contents) = body.get(at + header..at + len as usize) else {
            break;
        };
        out.push((kind, contents));
        at += len as usize;
    }
    out
}

fn child<'a>(body: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    children(body)
        .into_iter()
        .find(|(k, _)| k == kind)
        .map(|(_, b)| b)
}

/// The length (from `mvhd`) and the video track's picture size (its `tkhd`).
pub(crate) fn parse_moov(moov: &[u8]) -> Option<Skeleton> {
    let body = moov.get(8..)?;
    let mvhd = child(body, b"mvhd")?;
    let (timescale, duration) = match mvhd.first()? {
        1 => (
            u32::from_be_bytes(mvhd.get(20..24)?.try_into().ok()?),
            u64::from_be_bytes(mvhd.get(24..32)?.try_into().ok()?),
        ),
        _ => (
            u32::from_be_bytes(mvhd.get(12..16)?.try_into().ok()?),
            u64::from(u32::from_be_bytes(mvhd.get(16..20)?.try_into().ok()?)),
        ),
    };
    let (mut width, mut height) = (0, 0);
    for (kind, trak) in children(body) {
        if &kind != b"trak" {
            continue;
        }
        let video = child(trak, b"mdia")
            .and_then(|m| child(m, b"hdlr"))
            .and_then(|h| h.get(8..12))
            .is_some_and(|t| t == b"vide");
        let Some(tkhd) = child(trak, b"tkhd").filter(|_| video) else {
            continue;
        };
        // The last 8 bytes: width and height, 16.16 fixed point.
        let n = tkhd.len();
        if n >= 8 {
            width = u32::from_be_bytes(tkhd[n - 8..n - 4].try_into().ok()?) >> 16;
            height = u32::from_be_bytes(tkhd[n - 4..n].try_into().ok()?) >> 16;
            break;
        }
    }
    (timescale > 0).then(|| Skeleton::Video {
        duration: duration as f64 / f64::from(timescale),
        width,
        height,
    })
}

/// The page count: a linearized PDF says it at its start (`/N`); otherwise the page tree's
/// root, the `/Pages` dictionary with the largest `/Count`, at the start or the end.
async fn document(http: &Http, url: &str) -> Result<Option<Skeleton>> {
    let (remote, head) = http.probe_sample(url, PDF_HEAD, 1 << 40).await?;
    let (Some(size), Some(head)) = (remote.size, head) else {
        return Ok(None);
    };
    if let Some(pages) = linearized_pages(&head).or_else(|| page_count(&head)) {
        return Ok(Some(Skeleton::Document { pages }));
    }
    if size <= head.len() as u64 {
        return Ok(None);
    }
    let from = size.saturating_sub(PDF_TAIL).max(head.len() as u64);
    let tail = http.fetch(&remote, from, size, 1 << 40).await?;
    Ok(page_count(&tail).map(|pages| Skeleton::Document { pages }))
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| from + i)
}

/// The number after `key` (and spaces) in `text` at `at`.
fn number_after(text: &[u8], at: usize) -> Option<u32> {
    let digits: String = text
        .get(at..)?
        .iter()
        .skip_while(|b| b.is_ascii_whitespace())
        .take_while(|b| b.is_ascii_digit())
        .map(|b| char::from(*b))
        .collect();
    digits.parse().ok()
}

pub(crate) fn linearized_pages(head: &[u8]) -> Option<u32> {
    let start = &head[..head.len().min(2048)];
    let lin = find(start, b"/Linearized", 0)?;
    let end = find(start, b">>", lin).unwrap_or(start.len());
    let n = find(&start[..end], b"/N ", lin).or_else(|| find(&start[..end], b"/N", lin))?;
    number_after(start, n + 2).filter(|p| *p > 0)
}

/// The largest `/Count` of a `/Type /Pages` dictionary in `text`: the root of the page tree.
pub(crate) fn page_count(text: &[u8]) -> Option<u32> {
    let mut best = None;
    let mut pos = 0;
    while let Some(at) = find(text, b"/Pages", pos) {
        pos = at + 6;
        // `/Type /Pages` (or `/Type/Pages`), not a reference such as `/Pages 2 0 R`.
        let before = &text[at.saturating_sub(8)..at];
        if !before.ends_with(b"/Type ") && !before.ends_with(b"/Type") {
            continue;
        }
        let dict_start = text[..at].iter().rposition(|b| *b == b'<').unwrap_or(0);
        let dict_end = find(text, b">>", at).unwrap_or(text.len());
        let dict = &text[dict_start..dict_end];
        if let Some(c) = find(dict, b"/Count", 0).and_then(|c| number_after(dict, c + 6)) {
            best = Some(best.map_or(c, |b: u32| b.max(c)));
        }
    }
    best.filter(|p| *p > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_pages_of_a_pdf() {
        let pdf = shardload_testserver::samples::sample("pdf", 1).expect("pdf");
        assert_eq!(page_count(&pdf), Some(8));
        let lin = b"%PDF-1.5\n1 0 obj\n<< /Linearized 1 /L 12345 /H [ 1 2 ] /O 4 /E 99 /N 42 /T 77 >>\nendobj";
        assert_eq!(linearized_pages(lin), Some(42));
        assert_eq!(page_count(b"<< /Type /Catalog /Pages 2 0 R >>"), None);
    }

    #[test]
    fn reads_the_length_and_size_of_a_video() {
        for kind in ["mp4", "mp4end"] {
            let mp4 = shardload_testserver::samples::sample(kind, 1).expect(kind);
            let at = mp4.windows(4).position(|w| w == b"moov").expect("moov") - 4;
            let len = u32::from_be_bytes(mp4[at..at + 4].try_into().expect("len")) as usize;
            assert_eq!(
                parse_moov(&mp4[at..at + len]),
                Some(Skeleton::Video {
                    duration: 90.0,
                    width: 1280,
                    height: 720
                }),
                "{kind}"
            );
        }
    }

    #[test]
    fn a_large_box_shorter_than_its_own_header_is_not_read() {
        // `moov` holding a box whose 64-bit length (10) is less than its 16-byte header.
        let mut moov = vec![0, 0, 0, 32];
        moov.extend_from_slice(b"moov");
        moov.extend_from_slice(&[0, 0, 0, 1]);
        moov.extend_from_slice(b"mvhd");
        moov.extend_from_slice(&10u64.to_be_bytes());
        moov.extend_from_slice(&[0; 8]);
        assert_eq!(parse_moov(&moov), None);
    }
}
