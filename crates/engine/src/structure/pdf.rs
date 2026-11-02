//! A PDF's structure: the cross-reference tables (each object where they say it is) and its
//! compressed (Flate) streams, whose zlib checksum fails when a byte in them changed. Streams
//! are not checked in an encrypted file, nor where their length is not a plain number.

use super::Limit;
use crate::error::Result;

/// How far after an offset the object header (`12 0 obj`) must start, for whitespace.
const SLACK: usize = 8;
/// A cross-reference section chain longer than this is not followed (a loop, or hostile).
const MAX_SECTIONS: usize = 64;

pub(super) fn check(pdf: &[u8], limit: &Limit) -> Result<Option<Vec<(u64, u64)>>> {
    // Without a classic table (a cross-reference stream, PDF 1.5) the streams are still
    // checked.
    let mut damaged = xref(pdf, limit)?.unwrap_or_default();
    if find(pdf, b"/Encrypt", 0).is_none() {
        damaged.extend(streams(pdf, limit)?);
    }
    Ok(Some(damaged))
}

/// The classic cross-reference tables from `startxref` back through `/Prev`: every object
/// in use must start where its entry says. A wrong one is the entry and the place it points
/// to, since either may be the changed byte.
fn xref(pdf: &[u8], limit: &Limit) -> Result<Option<Vec<(u64, u64)>>> {
    let tail_from = pdf.len().saturating_sub(2048);
    let Some(at) = rfind(pdf, b"startxref", tail_from) else {
        return Ok(None);
    };
    let Some(mut section) = number_after(pdf, at + 9).map(|(n, _)| n) else {
        return Ok(Some(vec![(at as u64, pdf.len() as u64)]));
    };
    let mut damaged = Vec::new();
    for _ in 0..MAX_SECTIONS {
        limit.check()?;
        if section >= pdf.len() {
            return Ok(Some(vec![(at as u64, pdf.len() as u64)]));
        }
        if !pdf[section..].starts_with(b"xref") {
            // A cross-reference stream, or the offset is wrong: only the former is common
            // enough not to be called damage.
            return Ok(if object_at(pdf, section) {
                (!damaged.is_empty()).then_some(damaged)
            } else {
                Some(vec![(at as u64, pdf.len() as u64)])
            });
        }
        let mut pos = section + 4;
        // Subsections: `first count`, then `count` entries of 20 bytes.
        while let Some((_first, after)) = number_after(pdf, pos) {
            let Some((count, after)) = number_after(pdf, after) else {
                break;
            };
            let mut entry = skip_space(pdf, after);
            for _ in 0..count.min(1 << 24) {
                let Some(line) = pdf.get(entry..entry + 18) else {
                    return Ok(Some(vec![(section as u64, pdf.len() as u64)]));
                };
                let well_formed = line[..10].iter().all(u8::is_ascii_digit)
                    && line[11..16].iter().all(u8::is_ascii_digit)
                    && matches!(line[17], b'n' | b'f');
                if !well_formed {
                    // The table's own bytes are damaged; the rest of it cannot be read.
                    damaged.push((entry as u64, (section + 20 * count + 64) as u64));
                    return Ok(Some(damaged));
                }
                if line[17] == b'n'
                    && let Some(offset) = digits(&line[..10])
                    && offset > 0
                    && !object_at(pdf, offset)
                {
                    damaged.push((entry as u64, entry as u64 + 20));
                    damaged.push((offset as u64, (offset + 32) as u64));
                }
                entry = skip_space(pdf, entry + 18);
            }
            pos = entry;
        }
        // The trailer after this section says where the one before it is.
        let trailer = find(pdf, b"trailer", pos).filter(|t| *t < pos + 64);
        match trailer.and_then(|t| find(pdf, b"/Prev", t).filter(|p| *p < t + 4096)) {
            Some(prev) => match number_after(pdf, prev + 5) {
                Some((n, _)) => section = n,
                None => break,
            },
            None => break,
        }
    }
    Ok(Some(damaged))
}

/// Whether an object header (`12 0 obj`) starts at `offset`, give or take whitespace.
fn object_at(pdf: &[u8], offset: usize) -> bool {
    let Some(from) = pdf.get(offset..) else {
        return false;
    };
    let from = &from[..from.len().min(64)];
    let start = from
        .iter()
        .take(SLACK)
        .position(|b| !b.is_ascii_whitespace());
    let Some(start) = start else { return false };
    let Some((_, a)) = number_after(from, start) else {
        return false;
    };
    let Some((_, b)) = number_after(from, a) else {
        return false;
    };
    from[skip_space(from, b)..].starts_with(b"obj")
}

/// Every `stream` whose dictionary says `/FlateDecode` alone and gives `/Length` as a plain
/// number: its data must inflate without an error. The range is the object's dictionary
/// through the data.
fn streams(pdf: &[u8], limit: &Limit) -> Result<Vec<(u64, u64)>> {
    let mut damaged = Vec::new();
    let mut pos = 0;
    while let Some(at) = find(pdf, b"stream", pos) {
        pos = at + 6;
        if at >= 3 && &pdf[at - 3..at] == b"end" {
            continue;
        }
        limit.check()?;
        let Some(dict_start) = rfind(&pdf[..at], b"<<", at.saturating_sub(4096)) else {
            continue;
        };
        let dict = &pdf[dict_start..at];
        if !flate_only(dict) {
            continue;
        }
        let Some(len) = key(dict, b"/Length").and_then(|l| plain_number(dict, l)) else {
            continue;
        };
        let mut data = at + 6;
        if pdf.get(data) == Some(&b'\r') {
            data += 1;
        }
        if pdf.get(data) == Some(&b'\n') {
            data += 1;
        }
        let Some(stream) = pdf.get(data..data + len) else {
            continue;
        };
        // The length must be right (`endstream` follows) for the data to be judged.
        let after = skip_space(pdf, data + len);
        if !pdf[after..].starts_with(b"endstream") {
            continue;
        }
        if inflate_fails(stream) {
            let from = rfind(&pdf[..dict_start], b"obj", dict_start.saturating_sub(64))
                .unwrap_or(dict_start);
            damaged.push((from.min(dict_start) as u64, (after + 9) as u64));
        }
        pos = after + 9;
    }
    Ok(damaged)
}

/// `/Filter /FlateDecode` (or `[/FlateDecode]`), and no other filter.
fn flate_only(dict: &[u8]) -> bool {
    let Some(f) = key(dict, b"/Filter") else {
        return false;
    };
    let rest = &dict[skip_space(dict, f)..];
    match rest.strip_prefix(b"[") {
        Some(list) => list[skip_space(list, 0)..]
            .strip_prefix(b"/FlateDecode")
            .is_some_and(|r| r[skip_space(r, 0)..].starts_with(b"]")),
        None => rest
            .strip_prefix(b"/FlateDecode")
            .is_some_and(|r| !r.first().is_some_and(u8::is_ascii_alphanumeric)),
    }
}

/// Where the value of `name` starts in a dictionary: the name itself, not a longer one
/// (`/Length`, not `/Length1`).
fn key(dict: &[u8], name: &[u8]) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = find(dict, name, from) {
        let end = at + name.len();
        if !dict.get(end).is_some_and(u8::is_ascii_alphanumeric) {
            return Some(end);
        }
        from = end;
    }
    None
}

/// Whether zlib reports the data as damaged (a bad code or checksum). Data that ends early
/// is not called damaged: some writers cut the end off.
fn inflate_fails(data: &[u8]) -> bool {
    let mut z = flate2::Decompress::new(true);
    let mut out = vec![0u8; 256 << 10];
    let mut input = data;
    loop {
        let (read, written) = (z.total_in(), z.total_out());
        match z.decompress(input, &mut out, flate2::FlushDecompress::None) {
            Err(_) => return true,
            Ok(flate2::Status::StreamEnd) => return false,
            Ok(_) => {
                input = &input[(z.total_in() - read) as usize..];
                if z.total_in() == read && z.total_out() == written {
                    return false; // it wants more than there is
                }
            }
        }
    }
}

/// A number written as digits after optional whitespace at `pos`, and where it ends; not one
/// followed by ` 0 R` (a reference to another object).
fn plain_number(pdf: &[u8], pos: usize) -> Option<usize> {
    let (n, end) = number_after(pdf, pos)?;
    let rest = &pdf[end..];
    let reference =
        number_after(rest, 0).is_some_and(|(_, e)| rest[skip_space(rest, e)..].starts_with(b"R"));
    (!reference).then_some(n)
}

fn number_after(pdf: &[u8], pos: usize) -> Option<(usize, usize)> {
    let start = skip_space(pdf, pos);
    let len = pdf
        .get(start..)?
        .iter()
        .take(20)
        .take_while(|b| b.is_ascii_digit())
        .count();
    if len == 0 {
        return None;
    }
    Some((digits(&pdf[start..start + len])?, start + len))
}

fn digits(text: &[u8]) -> Option<usize> {
    std::str::from_utf8(text).ok()?.trim().parse().ok()
}

fn skip_space(pdf: &[u8], mut pos: usize) -> usize {
    while pdf.get(pos).is_some_and(u8::is_ascii_whitespace) {
        pos += 1;
    }
    pos
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| from + i)
}

/// The last `needle` at or after `from`.
fn rfind(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .rposition(|w| w == needle)
        .map(|i| from + i)
}
