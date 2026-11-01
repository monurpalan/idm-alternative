//! Deterministic file content.
//!
//! Every byte is a pure function of `(seed, offset)`, so the server can serve
//! files of any size without touching disk, and a download can be verified
//! byte-for-byte without storing a reference copy or a hash.

/// SplitMix64 finaliser: a cheap, well-distributed 64-bit mix.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn word(seed: u64, block: u64) -> [u8; 8] {
    mix(mix(seed) ^ block).to_le_bytes()
}

/// Fills `buf` with the content that starts at `offset` in the file identified by `seed`.
pub fn fill(seed: u64, offset: u64, buf: &mut [u8]) {
    let mut pos = offset;
    let mut i = 0;
    while i < buf.len() {
        let block = pos / 8;
        let within = (pos % 8) as usize;
        let w = word(seed, block);
        let take = (8 - within).min(buf.len() - i);
        buf[i..i + take].copy_from_slice(&w[within..within + take]);
        i += take;
        pos += take as u64;
    }
}

/// The first bytes of a file of `kind`, laid over the seeded content so a download can be
/// told apart by what it is: a program, a document, a web page. `None` for an unknown kind.
pub fn magic(kind: &str) -> Option<Vec<u8>> {
    Some(match kind {
        "exe" => {
            let mut b = vec![0u8; 0x84];
            b[..2].copy_from_slice(b"MZ");
            b[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
            b[0x80..].copy_from_slice(b"PE\0\0");
            b
        }
        "pdf" => b"%PDF-1.7\n".to_vec(),
        "html" => b"<!DOCTYPE html>\n<html><head><title>Sign in</title></head><body>".to_vec(),
        "zip" => {
            let mut b = b"PK\x03\x04".to_vec();
            b.resize(26, 0);
            b.extend_from_slice(b"\x0a\0\0\0readme.txt");
            b
        }
        "docx" => {
            let mut b = b"PK\x03\x04".to_vec();
            b.resize(26, 0);
            b.extend_from_slice(b"\x13\0\0\0[Content_Types].xmlPK\x03\x04word/document.xml");
            b
        }
        "png" => b"\x89PNG\r\n\x1A\n".to_vec(),
        "mp4" => b"\0\0\0\x20ftypisom".to_vec(),
        "sh" => b"#!/bin/sh\n".to_vec(),
        _ => return None,
    })
}

/// [`fill`], with the file starting as `magic` (see [`magic`]).
pub fn fill_with(seed: u64, magic: &[u8], offset: u64, buf: &mut [u8]) {
    fill(seed, offset, buf);
    for (i, b) in buf.iter_mut().enumerate() {
        match magic.get((offset + i as u64) as usize) {
            Some(m) => *b = *m,
            None => break,
        }
    }
}

/// Returns the offset of the first byte in `data` that differs from the expected
/// content starting at `offset`, if any.
pub fn first_mismatch(seed: u64, offset: u64, data: &[u8]) -> Option<u64> {
    let mut expected = vec![0u8; data.len()];
    fill(seed, offset, &mut expected);
    data.iter()
        .zip(&expected)
        .position(|(a, b)| a != b)
        .map(|i| offset + i as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_is_offset_consistent() {
        let mut whole = vec![0u8; 1000];
        fill(7, 0, &mut whole);
        for start in [0usize, 1, 7, 8, 9, 333, 999] {
            let mut part = vec![0u8; 1000 - start];
            fill(7, start as u64, &mut part);
            assert_eq!(part, whole[start..], "start={start}");
        }
    }

    #[test]
    fn seeds_differ() {
        let (mut a, mut b) = ([0u8; 64], [0u8; 64]);
        fill(1, 0, &mut a);
        fill(2, 0, &mut b);
        assert_ne!(a, b);
    }

    #[test]
    fn mismatch_reports_absolute_offset() {
        let mut data = vec![0u8; 100];
        fill(3, 50, &mut data);
        assert_eq!(first_mismatch(3, 50, &data), None);
        data[10] ^= 0xFF;
        assert_eq!(first_mismatch(3, 50, &data), Some(60));
    }
}
