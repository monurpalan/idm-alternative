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
