//! yt-dlp signs the checksum file of each release (`SHA2-256SUMS.sig`, OpenPGP). Its public
//! key is built into the app, so a release is set up only when the file listing its hashes
//! was signed by yt-dlp: a changed checksum file on a compromised download page cannot pass
//! with a program to match. (deno and BtbN's ffmpeg builds publish no signatures, only
//! checksums next to the files.)

use pgp::composed::{Deserializable, DetachedSignature, SignedPublicKey};

/// yt-dlp's release signing key (`public.key` in its repository), fingerprint
/// AC0C BBE6 848D 6A87 3464 AF4E 57CF 6593 3B5A 7581.
const YTDLP_KEY: &str = include_str!("../keys/yt-dlp.asc");

/// Whether `sig` (a binary or armored detached signature) is yt-dlp's over `sums`.
pub fn verify_ytdlp(sums: &[u8], sig: &[u8]) -> Result<(), String> {
    verify(YTDLP_KEY, sums, sig)
}

fn verify(key: &str, data: &[u8], sig: &[u8]) -> Result<(), String> {
    let (key, _) = SignedPublicKey::from_string(key).map_err(|e| format!("bad key: {e}"))?;
    let sig = if sig.starts_with(b"-----BEGIN") {
        DetachedSignature::from_armor_single(sig).map(|(s, _)| s)
    } else {
        DetachedSignature::from_bytes(sig)
    }
    .map_err(|e| format!("unreadable signature: {e}"))?;
    sig.verify(&key, data)
        .map_err(|e| format!("the signature does not match: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMS: &[u8] = include_bytes!("../keys/testdata/SHA2-256SUMS");
    const SIG: &[u8] = include_bytes!("../keys/testdata/SHA2-256SUMS.sig");

    #[test]
    fn a_real_release_is_signed_by_ytdlp() {
        verify_ytdlp(SUMS, SIG).expect("yt-dlp's signature");
    }

    #[test]
    fn a_changed_checksum_file_is_refused() {
        let mut changed = SUMS.to_vec();
        // One hash digit changed: a program swapped for another.
        let i = changed
            .iter()
            .position(|b| b.is_ascii_hexdigit())
            .expect("hex");
        changed[i] = if changed[i] == b'0' { b'1' } else { b'0' };
        assert!(verify_ytdlp(&changed, SIG).is_err());
        let mut longer = SUMS.to_vec();
        longer.extend_from_slice(b"0000  yt-dlp_evil.exe\n");
        assert!(verify_ytdlp(&longer, SIG).is_err());
    }

    #[test]
    fn another_key_or_a_broken_signature_is_refused() {
        let other = include_str!("../keys/testdata/not-yt-dlp.asc");
        assert!(
            verify(other, SUMS, SIG).is_err(),
            "signed by yt-dlp, not by this key"
        );
        let mut broken = SIG.to_vec();
        let last = broken.len() - 1;
        broken[last] ^= 0xff;
        assert!(verify_ytdlp(SUMS, &broken).is_err());
        assert!(verify_ytdlp(SUMS, b"not a signature").is_err());
        assert!(verify_ytdlp(SUMS, b"").is_err());
    }
}
