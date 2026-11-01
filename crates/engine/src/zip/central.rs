//! Reading an archive's index: the end records (ZIP64 too), the central directory's
//! entries, names in UTF-8 or code page 437, and little-endian numbers.

use super::*;

/// The end of central directory record, or what the ZIP64 record says instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct End {
    /// Where the record starts in the file.
    pub(crate) at: u64,
    pub(crate) entries: u64,
    pub(crate) central_size: u64,
    pub(crate) central_offset: u64,
    /// Where the archive says its ZIP64 end record is, when it has one.
    pub(crate) locator: Option<u64>,
}

/// Finds the end of central directory record in the last bytes of the file (`tail`, which
/// starts at `tail_start`).
pub(crate) fn parse_end(tail: &[u8], tail_start: u64) -> std::result::Result<End, String> {
    if tail.len() < EOCD_LEN {
        return Err("the file is too small to be a ZIP archive".into());
    }
    let found = |exact: bool| {
        (0..=tail.len() - EOCD_LEN).rev().find(|&i| {
            let comment = u16_at(tail, i + 20) as usize;
            u32_at(tail, i) == EOCD_SIG
                && if exact {
                    i + EOCD_LEN + comment == tail.len()
                } else {
                    i + EOCD_LEN + comment <= tail.len()
                }
        })
    };
    // A comment can hold the signature; a record whose comment ends the file is the one.
    let i = found(true)
        .or_else(|| found(false))
        .ok_or("no end of central directory: not a ZIP archive, or a damaged one")?;
    let r = &tail[i..];
    let locator = i
        .checked_sub(LOCATOR_LEN)
        .filter(|&l| u32_at(tail, l) == LOCATOR_SIG)
        .map(|l| u64_at(tail, l + 8));
    let saturated =
        u16_at(r, 10) == 0xFFFF || u32_at(r, 12) == u32::MAX || u32_at(r, 16) == u32::MAX;
    if saturated && locator.is_none() {
        return Err("a ZIP64 archive without its ZIP64 locator".into());
    }
    if u16_at(r, 4) != u16_at(r, 6) && locator.is_none() {
        return Err("archives split over several files are not supported".into());
    }
    Ok(End {
        at: tail_start + i as u64,
        entries: u64::from(u16_at(r, 10)),
        central_size: u64::from(u32_at(r, 12)),
        central_offset: u64::from(u32_at(r, 16)),
        locator,
    })
}

/// The ZIP64 end of central directory record (`at` is filled in by the caller).
pub(crate) fn parse_end64(r: &[u8]) -> std::result::Result<End, String> {
    if r.len() < EOCD64_LEN || u32_at(r, 0) != EOCD64_SIG {
        return Err("no ZIP64 end of central directory".into());
    }
    if u32_at(r, 16) != 0 || u32_at(r, 20) != 0 {
        return Err("archives split over several files are not supported".into());
    }
    Ok(End {
        at: 0,
        entries: u64_at(r, 32),
        central_size: u64_at(r, 40),
        central_offset: u64_at(r, 48),
        locator: None,
    })
}

/// Reads `count` central directory records; `shift` is added to every local header offset.
pub(crate) fn parse_central(
    buf: &[u8],
    count: u64,
    shift: u64,
) -> std::result::Result<Vec<ZipEntry>, String> {
    if count.saturating_mul(CENTRAL_LEN as u64) > buf.len() as u64 {
        return Err(format!(
            "{count} entries cannot fit in the central directory"
        ));
    }
    let mut entries = Vec::with_capacity(count as usize);
    let mut at = 0;
    for index in 0..count as usize {
        let r = buf
            .get(at..at + CENTRAL_LEN)
            .ok_or("the central directory is cut short")?;
        if u32_at(r, 0) != CENTRAL_SIG {
            return Err(format!("entry {index} of the central directory is damaged"));
        }
        let flags = u16_at(r, 8);
        let method = u16_at(r, 10);
        let crc32 = u32_at(r, 16);
        let mut compressed_size = u64::from(u32_at(r, 20));
        let mut size = u64::from(u32_at(r, 24));
        let name_len = u16_at(r, 28) as usize;
        let extra_len = u16_at(r, 30) as usize;
        let comment_len = u16_at(r, 32) as usize;
        let mut header_offset = u64::from(u32_at(r, 42));
        let name_at = at + CENTRAL_LEN;
        let extra_at = name_at + name_len;
        let next = extra_at + extra_len + comment_len;
        if next > buf.len() {
            return Err("the central directory is cut short".into());
        }
        let raw_name = &buf[name_at..extra_at];
        let mut name = None;
        let mut extra = &buf[extra_at..extra_at + extra_len];
        while extra.len() >= 4 {
            let (id, len) = (u16_at(extra, 0), u16_at(extra, 2) as usize);
            let Some(field) = extra.get(4..4 + len) else {
                break;
            };
            match id {
                // ZIP64: the values the record left at their maximum, in this order.
                0x0001 => {
                    let mut f = field;
                    let mut next64 = |v: &mut u64, max: bool| {
                        if max && f.len() >= 8 {
                            *v = u64_at(f, 0);
                            f = &f[8..];
                        }
                    };
                    let max = u64::from(u32::MAX);
                    let wanted = (size == max, compressed_size == max, header_offset == max);
                    next64(&mut size, wanted.0);
                    next64(&mut compressed_size, wanted.1);
                    next64(&mut header_offset, wanted.2);
                }
                // Info-ZIP Unicode path: the UTF-8 name, if the name itself is not.
                0x7075 if field.len() > 5 && flags & (1 << 11) == 0 => {
                    name = std::str::from_utf8(&field[5..]).ok().map(str::to_owned);
                }
                _ => {}
            }
            extra = &extra[4 + len..];
        }
        let name = name.unwrap_or_else(|| decode_name(raw_name, flags & (1 << 11) != 0));
        entries.push(ZipEntry {
            index,
            is_dir: name.ends_with('/') || name.ends_with('\\'),
            name,
            compressed_size,
            size,
            method,
            crc32,
            header_offset: header_offset + shift,
            encrypted: flags & 1 != 0,
        });
        at = next;
    }
    Ok(entries)
}

/// Names are UTF-8 when the archive says so (or when they are valid UTF-8), else code page
/// 437 as in DOS.
pub(crate) fn decode_name(raw: &[u8], utf8: bool) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_owned(),
        Err(_) if utf8 => String::from_utf8_lossy(raw).into_owned(),
        Err(_) => raw
            .iter()
            .map(|&b| {
                if b < 0x80 {
                    b as char
                } else {
                    CP437_HIGH.chars().nth(usize::from(b - 0x80)).unwrap_or('_')
                }
            })
            .collect(),
    }
}

pub(crate) const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

pub(crate) fn u16_at(b: &[u8], i: usize) -> u16 {
    b.get(i..i + 2)
        .map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}

pub(crate) fn u32_at(b: &[u8], i: usize) -> u32 {
    b.get(i..i + 4)
        .map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

pub(crate) fn u64_at(b: &[u8], i: usize) -> u64 {
    b.get(i..i + 8).map_or(0, |s| {
        u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
    })
}
