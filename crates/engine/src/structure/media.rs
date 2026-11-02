//! Video containers. MP4 has no checksums: its boxes must chain to the end of the file and
//! those inside `moov` (where the index of the media is) must fit their parents, so a
//! changed size or type shows; a changed byte in the media data does not. Matroska's elements
//! must fit their parents too, and the CRC-32 elements writers put in clusters and the like
//! are checked.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use super::{Limit, read_at};
use crate::error::Result;

/// Boxes inside these hold boxes.
const MP4_CONTAINERS: [&[u8; 4]; 11] = [
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"dinf", b"mvex", b"moof", b"traf",
    b"udta",
];
/// Nesting deeper than this is not followed.
const MAX_DEPTH: usize = 16;

/// A box header: its type and where its contents start and it ends.
struct Mp4Box {
    kind: [u8; 4],
    body: u64,
    end: u64,
}

/// The box at `at`, within `end`; `Err(())` when its header is not one.
fn mp4_box(file: &mut File, size: u64, at: u64, end: u64) -> std::result::Result<Mp4Box, ()> {
    let head = read_at(file, size, at, 8).ok_or(())?;
    let kind = [head[4], head[5], head[6], head[7]];
    if !kind.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        return Err(());
    }
    let short = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
    let (len, header) = match short {
        // To the end of its parent.
        0 => (end - at, 8),
        1 => {
            let large = read_at(file, size, at + 8, 8).ok_or(())?;
            let mut b = [0u8; 8];
            b.copy_from_slice(&large);
            (u64::from_be_bytes(b), 16)
        }
        n => (n, 8),
    };
    if len < header || at.checked_add(len).is_none_or(|e| e > end) {
        return Err(());
    }
    Ok(Mp4Box {
        kind,
        body: at + header,
        end: at + len,
    })
}

pub(super) fn mp4(file: &mut File, size: u64, limit: &Limit) -> Result<Option<Vec<(u64, u64)>>> {
    let mut damaged = Vec::new();
    let mut moov = false;
    mp4_children(file, size, 0, size, 0, limit, &mut damaged, &mut moov)?;
    // Without its index the file was not understood well enough to say it is sound.
    Ok((moov || !damaged.is_empty()).then_some(damaged))
}

/// Walks the boxes in `from..end`. A header that is not one is damage, and so is the header
/// before it: a changed size there sends the walk to the wrong place.
#[allow(clippy::too_many_arguments, reason = "one walk's state, passed down")]
fn mp4_children(
    file: &mut File,
    size: u64,
    from: u64,
    end: u64,
    depth: usize,
    limit: &Limit,
    damaged: &mut Vec<(u64, u64)>,
    moov: &mut bool,
) -> Result<()> {
    let mut at = from;
    let mut previous: Option<u64> = None;
    while at + 8 <= end {
        limit.check()?;
        let Ok(b) = mp4_box(file, size, at, end) else {
            // Trailing zeros after the last box are padding some writers leave.
            if depth == 0 && zeros(file, size, at, end) {
                return Ok(());
            }
            if let Some(p) = previous {
                damaged.push((p, p + 16));
            }
            damaged.push((at, (at + 16).min(end)));
            return Ok(());
        };
        if &b.kind == b"moov" {
            *moov = true;
        }
        if depth < MAX_DEPTH && MP4_CONTAINERS.contains(&&b.kind) {
            mp4_children(file, size, b.body, b.end, depth + 1, limit, damaged, moov)?;
        }
        previous = Some(at);
        at = b.end;
    }
    if at != end && depth > 0 {
        // A few bytes left over inside a parent: its size or the last child's is wrong.
        damaged.push((from.saturating_sub(16), from));
    }
    Ok(())
}

/// Whether `at..end` holds only zeros (read up to 1 MiB).
fn zeros(file: &mut File, size: u64, at: u64, end: u64) -> bool {
    let len = (end - at).min(1 << 20) as usize;
    read_at(file, size, at, len).is_some_and(|b| b.iter().all(|x| *x == 0)) && end - at <= 1 << 20
}

/// Matroska element IDs whose children are checked.
const EBML_MASTERS: [u32; 9] = [
    0x1853_8067, // Segment
    0x114D_9B74, // SeekHead
    0x1549_A966, // Info
    0x1654_AE6B, // Tracks
    0x1F43_B675, // Cluster
    0x1C53_BB6B, // Cues
    0x1254_C367, // Tags
    0x1941_A469, // Attachments
    0x1043_A770, // Chapters
];
const EBML_CRC: u32 = 0xBF;
const EBML_UNKNOWN: u64 = u64::MAX;

/// An element header: its ID, where its data starts and its size (`EBML_UNKNOWN` when the
/// writer did not know it, as live recordings leave clusters).
fn ebml(file: &mut File, size: u64, at: u64) -> Option<(u32, u64, u64)> {
    let head = read_at(file, size, at, 12.min((size - at) as usize))?;
    let (id, id_len) = vint(&head, true)?;
    let (len, len_len) = vint(&head[id_len..], false)?;
    Some((id as u32, at + (id_len + len_len) as u64, len))
}

/// A variable-length integer: an element ID keeps its length marker, a size does not; a size
/// of all ones is "unknown".
fn vint(b: &[u8], id: bool) -> Option<(u64, usize)> {
    let first = *b.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > if id { 4 } else { 8 } || b.len() < len {
        return None;
    }
    let marker = 0xFFu8.checked_shr(len as u32).unwrap_or(0);
    let mut v = u64::from(if id { first } else { first & marker });
    for x in &b[1..len] {
        v = (v << 8) | u64::from(*x);
    }
    let all_ones = !id && v == (1u64 << (7 * len)) - 1;
    Some((if all_ones { EBML_UNKNOWN } else { v }, len))
}

pub(super) fn matroska(
    file: &mut File,
    size: u64,
    limit: &Limit,
) -> Result<Option<Vec<(u64, u64)>>> {
    let mut damaged = Vec::new();
    let known = ebml_children(file, size, 0, size, 0, limit, &mut damaged)?;
    Ok((known || !damaged.is_empty()).then_some(damaged))
}

/// Walks the elements in `from..end`; whether it got to the end (an element of unknown size
/// stops it, and then the file was only partly checked).
fn ebml_children(
    file: &mut File,
    size: u64,
    from: u64,
    end: u64,
    depth: usize,
    limit: &Limit,
    damaged: &mut Vec<(u64, u64)>,
) -> Result<bool> {
    let mut at = from;
    while at < end {
        limit.check()?;
        let Some((id, body, len)) = ebml(file, size, at) else {
            damaged.push((at, (at + 12).min(end)));
            return Ok(true);
        };
        if len == EBML_UNKNOWN {
            return Ok(false);
        }
        let Some(next) = body.checked_add(len).filter(|n| *n <= end) else {
            damaged.push((at, (at + 12).min(end)));
            return Ok(true);
        };
        if depth < MAX_DEPTH && EBML_MASTERS.contains(&id) {
            if !crc_holds(file, size, body, next)? {
                damaged.push((at, next));
            } else if !ebml_children(file, size, body, next, depth + 1, limit, damaged)? {
                return Ok(false);
            }
        }
        at = next;
    }
    Ok(true)
}

/// When an element's first child is a CRC-32, whether it matches the rest of the element.
fn crc_holds(file: &mut File, size: u64, body: u64, end: u64) -> Result<bool> {
    let Some((EBML_CRC, crc_at, 4)) = ebml(file, size, body).filter(|_| body < end) else {
        return Ok(true);
    };
    let Some(stored) = read_at(file, size, crc_at, 4) else {
        return Ok(false);
    };
    let mut crc = crc32fast::Hasher::new();
    file.seek(SeekFrom::Start(crc_at + 4))?;
    let mut rest = (&mut *file).take(end.saturating_sub(crc_at + 4));
    let mut buf = vec![0u8; 256 << 10];
    loop {
        let n = rest.read(&mut buf)?;
        if n == 0 {
            break;
        }
        crc.update(&buf[..n]);
    }
    Ok(u32::from_le_bytes([stored[0], stored[1], stored[2], stored[3]]) == crc.finalize())
}
