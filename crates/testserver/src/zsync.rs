//! `.zsync` files, as zsyncmake writes them (without compression), for testing delta
//! downloads: a header, then per block the last `weak` bytes of its rsum (`a`, `b`, big-endian)
//! and the first `strong` bytes of its MD4, a short last block padded with zeros.

use md4::{Digest, Md4};
use sha1::Sha1;

/// The `.zsync` of `data`, in blocks of `blocksize`, its file at `url`.
pub fn make(
    data: &[u8],
    name: &str,
    url: &str,
    blocksize: usize,
    weak: usize,
    strong: usize,
) -> Vec<u8> {
    let sha1: String = Sha1::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut out = format!(
        "zsync: 0.6.2\nFilename: {name}\nMTime: Tue, 03 Nov 2026 00:00:00 +0000\nBlocksize: {blocksize}\nLength: {}\nHash-Lengths: 2,{weak},{strong}\nURL: {url}\nSHA-1: {sha1}\n\n",
        data.len()
    )
    .into_bytes();
    for chunk in data.chunks(blocksize) {
        let mut block = chunk.to_vec();
        block.resize(blocksize, 0);
        let (mut a, mut b) = (0u16, 0u16);
        for (i, &c) in block.iter().enumerate() {
            a = a.wrapping_add(u16::from(c));
            b = b.wrapping_add(((blocksize - i) as u16).wrapping_mul(u16::from(c)));
        }
        let rsum = [a.to_be_bytes(), b.to_be_bytes()].concat();
        out.extend_from_slice(&rsum[4 - weak..]);
        out.extend_from_slice(&Md4::digest(&block)[..strong]);
    }
    out
}
