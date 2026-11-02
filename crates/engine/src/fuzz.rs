//! Entry points for the fuzz targets in `fuzz/`: each feeds untrusted bytes to one parser and
//! checks what its output promises. Built with `--cfg fuzzing` (cargo-fuzz sets it) and for
//! the tests, which replay the inputs that once failed (`fuzz/regressions/<target>/`).

use std::path::Component;
use std::sync::atomic::AtomicBool;

use crate::name::{self, MAX_UNITS};

/// A media or master playlist, and what is read out of its segments.
pub fn hls(text: &str) {
    let url = "https://example.com/a/b/index.m3u8";
    if let Ok(crate::hls::Playlist::Media(list)) = crate::hls::parse_any(url, text) {
        for s in &list.segments {
            if let Some((start, end)) = s.range {
                assert!(start <= end, "byte range {start}..{end}");
            }
        }
        let clip = crate::hls::Clip {
            start: 1.5,
            end: 7.0,
            exact: false,
        };
        let (kept, _) = crate::hls::window(&list, clip);
        assert!(kept.iter().all(|&i| i < list.segments.len()));
    }
    for line in text.lines() {
        let _ = crate::hls::byte_range(line, 0);
        let _ = crate::hls::hex_iv(line);
    }
}

/// A ZIP archive's end records and central directory, read the way [`crate::LocalZip`] does,
/// and every entry's name turned into a path inside the target folder.
pub fn zip(data: &[u8], scratch: &std::path::Path) {
    if let Ok(end) = crate::zip::central::parse_end(data, 0) {
        let _ = crate::zip::central::parse_central(data, end.entries, 0);
    }
    let _ = crate::zip::central::parse_end64(data);
    if std::fs::write(scratch, data).is_err() {
        return;
    }
    if let Ok(zip) = crate::LocalZip::open(scratch) {
        for e in zip.entries() {
            let _ = e.unsupported();
            safe_path(&e.name);
        }
    }
}

/// A path that stays inside the folder: only plain parts, each a valid file name.
pub fn safe_path(name: &str) {
    let Some(path) = crate::zip::safe_path(name) else {
        return;
    };
    for part in path.components() {
        let Component::Normal(part) = part else {
            panic!("{name:?} gave {path:?}");
        };
        let part = part.to_str().expect("built from a str");
        assert_eq!(name::sanitize(part), part, "{name:?} gave {path:?}");
    }
}

/// A file name from a header, an address or anywhere else, made safe to create.
pub fn file_name(text: &str) {
    let clean = name::sanitize(text);
    assert!(!clean.is_empty(), "{text:?}");
    assert!(
        !clean.contains(['/', '\\', '<', '>', ':', '"', '|', '?', '*']),
        "{text:?} gave {clean:?}"
    );
    assert!(
        !clean.chars().any(char::is_control),
        "{text:?} gave {clean:?}"
    );
    assert!(!clean.ends_with(['.', ' ']), "{text:?} gave {clean:?}");
    assert!(clean.encode_utf16().count() <= MAX_UNITS, "{text:?}");
    assert!(!name::is_reserved(&clean), "{text:?} gave {clean:?}");
    assert_eq!(name::sanitize(&clean), clean, "{text:?} is not stable");
    let _ = name::from_content_disposition(text.as_bytes());
    let _ = name::from_url(text);
}

/// The name a download gets from its headers and addresses.
pub fn chosen_name(text: &str) {
    let mut parts = text.splitn(3, '\n');
    let (cd, ct, url) = (parts.next(), parts.next(), parts.next().unwrap_or_default());
    let chosen = name::choose(cd.map(str::as_bytes), ct, url, url);
    file_name(&chosen);
}

/// A Metalink document.
pub fn metalink(text: &str) {
    if let Ok(m) = crate::metalink::parse(text) {
        let _ = name::sanitize(&m.name);
    }
}

/// The first bytes of a file, the type they say it is, and what that means for its name.
pub fn sniff(head: &[u8], name: &str) {
    let content = crate::sniff::sniff(head);
    let _ = crate::sniff::check(name, content);
    if let Some(better) = crate::sniff::better_name(name, content, Some(name)) {
        file_name(&better);
    }
    let _ = crate::sniff::kind_of_name(name);
    let _ = crate::sniff::disguised_name(name);
}

/// A file's structure checked (ZIP, gzip, xz, PNG, PDF, MP4, Matroska), and the skeleton
/// read from its first bytes.
pub fn structure(data: &[u8], scratch: &std::path::Path) {
    let _ = crate::skeleton::parse_moov(data);
    let _ = crate::skeleton::linearized_pages(data);
    let _ = crate::skeleton::page_count(data);
    if std::fs::write(scratch, data).is_ok() {
        let _ = crate::structure::check(scratch, &AtomicBool::new(false));
    }
}

/// A `Zone.Identifier` stream, and one written for an address.
pub fn zone(text: &str) {
    #[cfg(windows)]
    let _ = crate::zone::parse_zone_identifier(text);
    let mut halves = text.splitn(2, '\n');
    let url = halves.next().unwrap_or_default();
    if let Some(written) = crate::zone::zone_identifier(url, halves.next()) {
        assert!(written.starts_with("[ZoneTransfer]"), "{written:?}");
        let lines = written.split("\r\n").filter(|l| !l.is_empty()).count();
        let joined = written.replace("\r\n", "");
        assert!(lines <= 4 && !joined.contains(['\r', '\n']), "{written:?}");
    }
}

/// Checksums: typed by the user, listed in a `SHA256SUMS`-like file, or in GitHub's answer.
pub fn checksum(text: &str) {
    let _ = text.parse::<crate::Checksum>();
    let names = ["file.iso".to_owned(), "file-1.2.zip".to_owned()];
    let _ = crate::discover::find_in_sum_file(text, &names, false);
    let _ = crate::discover::find_in_sum_file(text, &names, true);
    let _ = crate::discover::digest_of(text, "file.iso");
    let _ = crate::discover::github_release(text);
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// The inputs kept under `fuzz/regressions/<target>/`.
    fn inputs(target: &str) -> Vec<Vec<u8>> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fuzz/regressions")
            .join(target);
        let Ok(read) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        read.map(|e| std::fs::read(e.expect("a regression input").path()))
            .collect::<std::io::Result<_>>()
            .expect("a regression input")
    }

    fn text(data: &[u8]) -> &str {
        std::str::from_utf8(data).unwrap_or_default()
    }

    #[test]
    fn fuzz_regressions() {
        let scratch =
            std::env::temp_dir().join(format!("shardload-fuzz-test-{}", std::process::id()));
        for data in inputs("hls") {
            super::hls(text(&data));
        }
        for data in inputs("zip") {
            super::zip(&data, &scratch);
        }
        for data in inputs("metalink") {
            super::metalink(text(&data));
        }
        for data in inputs("file_name") {
            super::file_name(text(&data));
            super::chosen_name(text(&data));
            super::safe_path(text(&data));
        }
        for data in inputs("sniff") {
            let (head, name) = data.split_at(data.len().min(64));
            super::sniff(head, &String::from_utf8_lossy(name));
        }
        for data in inputs("structure") {
            super::structure(&data, &scratch);
        }
        for data in inputs("zone") {
            super::zone(text(&data));
        }
        for data in inputs("checksum") {
            super::checksum(text(&data));
        }
        let _ = std::fs::remove_file(&scratch);
    }
}
