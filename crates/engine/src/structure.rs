//! Checking a finished file without a checksum by its own structure: a ZIP's CRC-32s, gzip's
//! and xz's own checks, PNG's chunk CRCs, a PDF's cross-reference table and compressed
//! streams, the boxes of an MP4 and the elements (and CRC-32s) of a Matroska file. A damaged
//! part comes back as a byte range, so only that range is downloaded again.
//!
//! The file is untrusted and the checks must never call a sound file damaged: only a check
//! that fails (a CRC, a decompressor's error, a record pointing nowhere) counts. What a check
//! does not understand (encryption, a compression method it cannot unpack, a PDF whose
//! streams' lengths are not plain numbers) is left alone. Reads are bounded: files are
//! streamed through fixed buffers, a PDF over [`MAX_PDF`] is not read, and the whole check
//! gives up after [`BUDGET`].

mod media;
mod pdf;
mod repair;

pub use repair::Checked;
pub(crate) use repair::repair;

use std::fs::File;
use std::io::{BufRead as _, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// How long a check may take before it gives up (and says nothing).
const BUDGET: Duration = Duration::from_secs(120);
/// PDFs are read whole; a larger one is not checked.
const MAX_PDF: u64 = 256 << 20;
/// The memory an xz stream may ask for, in KiB.
const XZ_MEMORY_KB: u32 = 256 << 10;

/// What a check of a file's structure found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Every check it has passed.
    Sound(Format),
    /// These byte ranges (`start..end`) failed their checks.
    Damaged(Format, Vec<(u64, u64)>),
    /// Not a format it checks, or it could not tell (too large, too slow, encrypted).
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    Gzip,
    Xz,
    Png,
    Pdf,
    Mp4,
    Matroska,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Zip => "ZIP",
            Format::Gzip => "gzip",
            Format::Xz => "xz",
            Format::Png => "PNG",
            Format::Pdf => "PDF",
            Format::Mp4 => "MP4",
            Format::Matroska => "Matroska",
        }
    }

    /// The format a file's first bytes say it is.
    fn of(head: &[u8]) -> Option<Self> {
        Some(match head {
            [b'P', b'K', 3, 4, ..] | [b'P', b'K', 5, 6, ..] => Format::Zip,
            [0x1F, 0x8B, ..] => Format::Gzip,
            [0xFD, b'7', b'z', b'X', b'Z', 0, ..] => Format::Xz,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n', ..] => Format::Png,
            [b'%', b'P', b'D', b'F', b'-', ..] => Format::Pdf,
            [_, _, _, _, b'f', b't', b'y', b'p', ..] => Format::Mp4,
            [0x1A, 0x45, 0xDF, 0xA3, ..] => Format::Matroska,
            _ => return None,
        })
    }
}

/// When a check must stop: asked to, or out of time.
pub(crate) struct Limit<'a> {
    stop: &'a AtomicBool,
    until: Instant,
}

impl Limit<'_> {
    pub(crate) fn check(&self) -> Result<()> {
        if self.stop.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else if Instant::now() > self.until {
            Err(Error::Stalled(BUDGET))
        } else {
            Ok(())
        }
    }
}

/// Checks the file at `path` by its structure. Reads it on the calling thread: call it from
/// a blocking task. `stop` set ends it with [`Error::Cancelled`].
pub fn check(path: &Path, stop: &AtomicBool) -> Result<Verdict> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut head = [0u8; 16];
    let n = file.read(&mut head)?;
    let Some(format) = Format::of(&head[..n]) else {
        return Ok(Verdict::Unknown);
    };
    let limit = Limit {
        stop,
        until: Instant::now() + BUDGET,
    };
    let found = match format {
        Format::Zip => zip(path, &limit),
        Format::Gzip => gzip(&mut file, size, &limit),
        Format::Xz => xz(file, size, &limit),
        Format::Png => png(&mut file, size, &limit),
        Format::Pdf if size <= MAX_PDF => pdf::check(&std::fs::read(path)?, &limit),
        Format::Pdf => return Ok(Verdict::Unknown),
        Format::Mp4 => media::mp4(&mut file, size, &limit),
        Format::Matroska => media::matroska(&mut file, size, &limit),
    };
    match found {
        Ok(Some(ranges)) if ranges.is_empty() => Ok(Verdict::Sound(format)),
        Ok(Some(ranges)) => Ok(Verdict::Damaged(format, merge(ranges, size))),
        Ok(None) | Err(Error::Stalled(_)) => Ok(Verdict::Unknown),
        Err(e) => Err(e),
    }
}

/// Sorted, clamped to the file and overlapping ranges joined.
fn merge(mut ranges: Vec<(u64, u64)>, size: u64) -> Vec<(u64, u64)> {
    ranges.retain_mut(|(s, e)| {
        *e = (*e).min(size);
        s < e
    });
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (s, e) in ranges {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// The entries whose CRC-32 fails; the list of files itself, when it cannot be read.
fn zip(path: &Path, limit: &Limit) -> Result<Option<Vec<(u64, u64)>>> {
    let size = std::fs::metadata(path)?.len();
    let mut zip = match crate::zip::LocalZip::open(path) {
        Ok(z) => z,
        // Its end records or list of files are broken: they are at the end of the file.
        Err(Error::Archive(_)) => return Ok(Some(vec![(size.saturating_sub(1 << 20), size)])),
        Err(e) => return Err(e),
    };
    let mut damaged = zip.damaged_entries(&|| limit.check())?;
    // A list of files that no longer matches the entries (a flipped byte in it) shows up as
    // entries that fail; the list itself is re-read with them.
    if !damaged.is_empty() {
        damaged.push(zip.central());
    }
    Ok(Some(damaged))
}

/// Each member's CRC-32 and size (its trailer) after unpacking it.
fn gzip(file: &mut File, size: u64, limit: &Limit) -> Result<Option<Vec<(u64, u64)>>> {
    let mut pos = 0u64;
    let mut damaged = Vec::new();
    while pos < size {
        limit.check()?;
        file.seek(SeekFrom::Start(pos))?;
        let mut reader = BufReader::with_capacity(256 << 10, &mut *file);
        let Some(header) = gzip_header(&mut reader)? else {
            // Not another member: the rest must be padding (zeros), or it is damaged.
            let mut rest = Vec::new();
            reader.take(1 << 20).read_to_end(&mut rest)?;
            if rest.iter().any(|b| *b != 0) {
                damaged.push((pos, size));
            }
            break;
        };
        let mut inflate = flate2::bufread::DeflateDecoder::new(reader);
        let mut crc = crc32fast::Hasher::new();
        let mut len = 0u64;
        let mut buf = vec![0u8; 256 << 10];
        let ok = loop {
            match inflate.read(&mut buf) {
                Ok(0) => break true,
                Ok(n) => {
                    crc.update(&buf[..n]);
                    len += n as u64;
                }
                Err(_) => break false,
            }
            if len % (64 << 20) < (256 << 10) {
                limit.check()?;
            }
        };
        let trailer_at = pos + header + inflate.total_in();
        let trailer = read_at(file, size, trailer_at, 8);
        let sound = ok
            && trailer.as_ref().is_some_and(|t| {
                u32::from_le_bytes([t[0], t[1], t[2], t[3]]) == crc.clone().finalize()
                    && u32::from_le_bytes([t[4], t[5], t[6], t[7]]) == len as u32
            });
        if !sound {
            // Where the member ends is not known once it is damaged: to the end.
            damaged.push((pos, size));
            break;
        }
        pos = trailer_at + 8;
    }
    Ok(Some(damaged))
}

/// A gzip member's header length, or `None` when the bytes are not one.
fn gzip_header(r: &mut impl std::io::BufRead) -> Result<Option<u64>> {
    let mut fixed = [0u8; 10];
    if r.read_exact(&mut fixed).is_err() || fixed[..3] != [0x1F, 0x8B, 8] {
        return Ok(None);
    }
    let flags = fixed[3];
    let mut len = 10u64;
    if flags & 4 != 0 {
        let mut x = [0u8; 2];
        r.read_exact(&mut x)?;
        let extra = u64::from(u16::from_le_bytes(x));
        std::io::copy(&mut r.take(extra), &mut std::io::sink())?;
        len += 2 + extra;
    }
    for bit in [8, 16] {
        if flags & bit != 0 {
            let mut text = Vec::new();
            r.take(64 << 10).read_until(0, &mut text)?;
            len += text.len() as u64;
        }
    }
    if flags & 2 != 0 {
        std::io::copy(&mut r.take(2), &mut std::io::sink())?;
        len += 2;
    }
    Ok(Some(len))
}

/// The stream's own checks (CRC-32, CRC-64 or SHA-256 per block, and its index). Where in
/// the stream the damage is is not worked out: the whole file is the range.
fn xz(mut file: File, size: u64, limit: &Limit) -> Result<Option<Vec<(u64, u64)>>> {
    file.seek(SeekFrom::Start(0))?;
    let mut reader = lzma_rust2::XzReader::new_mem_limit(
        BufReader::with_capacity(256 << 10, file),
        true,
        XZ_MEMORY_KB,
    );
    let mut buf = vec![0u8; 256 << 10];
    let mut since = 0u64;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(Some(Vec::new())),
            Ok(n) => {
                since += n as u64;
                if since > 64 << 20 {
                    since = 0;
                    limit.check()?;
                }
            }
            // Asking for more memory than allowed says nothing about damage.
            Err(e) if e.to_string().contains("mem_limit") => return Ok(None),
            Err(_) => return Ok(Some(vec![(0, size)])),
        }
    }
}

/// Every chunk's CRC-32, up to `IEND`.
fn png(file: &mut File, size: u64, limit: &Limit) -> Result<Option<Vec<(u64, u64)>>> {
    let mut pos = 8u64;
    let mut buf = vec![0u8; 256 << 10];
    while pos < size {
        limit.check()?;
        let Some(head) = read_at(file, size, pos, 8) else {
            return Ok(Some(vec![(pos, size)]));
        };
        let len = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
        let end = pos + 12 + len;
        if end > size || !head[4..8].iter().all(u8::is_ascii_alphabetic) {
            // A length or a type that is not one: this chunk's header.
            return Ok(Some(vec![(pos, (pos + 12).min(size))]));
        }
        let mut crc = crc32fast::Hasher::new();
        crc.update(&head[4..8]);
        let mut left = len;
        while left > 0 {
            let n = left.min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..n])?;
            crc.update(&buf[..n]);
            left -= n as u64;
        }
        let mut stored = [0u8; 4];
        file.read_exact(&mut stored)?;
        if u32::from_be_bytes(stored) != crc.finalize() {
            return Ok(Some(vec![(pos, end)]));
        }
        if &head[4..8] == b"IEND" {
            return Ok(Some(Vec::new()));
        }
        pos = end;
    }
    // No IEND: cut short, or a length pointing past it.
    Ok(None)
}

/// `len` bytes at `at`, or `None` when the file has fewer.
pub(crate) fn read_at(file: &mut File, size: u64, at: u64, len: usize) -> Option<Vec<u8>> {
    if at.checked_add(len as u64)? > size {
        return None;
    }
    file.seek(SeekFrom::Start(at)).ok()?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).ok()?;
    Some(buf)
}
