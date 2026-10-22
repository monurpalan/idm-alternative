//! Metalink: one file with its size, checksum and the addresses it can be downloaded from
//! (Metalink 4, RFC 5854, `.meta4`; and the older Metalink 3, `.metalink`). Mirror networks
//! such as openSUSE's offer one next to each file, its mirrors sorted for the client.

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

use crate::download::{Options, fetch_text};
use crate::error::{Error, Result};

/// The most a metalink may weigh: piece hashes of a large file run to some hundred KiB.
const MAX_SIZE: usize = 4 << 20;

/// The most addresses taken from a metalink: each is checked before use, and more sources
/// than connections only add checks.
pub const MAX_URLS: usize = 10;

/// What a metalink says about its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metalink {
    pub name: String,
    pub size: Option<u64>,
    /// `algorithm:hex` of the strongest hash given, for [`crate::Checksum`].
    pub checksum: Option<String>,
    /// HTTP(S) addresses of the file, the preferred first.
    pub urls: Vec<String>,
    /// Hashes of its pieces, when given (`<pieces>`): a corrupt piece is then downloaded
    /// again alone.
    pub pieces: Option<crate::pieces::Pieces>,
}

/// Whether `url` names a metalink file (by its extension).
pub fn is_metalink(url: &str) -> bool {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    path.ends_with(".meta4") || path.ends_with(".metalink")
}

/// Downloads and reads the metalink at `url`.
pub async fn fetch_metalink(
    url: &str,
    headers: &[(String, String)],
    opts: &Options,
) -> Result<Metalink> {
    let (text, _) = fetch_text(url, headers, opts, MAX_SIZE).await?;
    parse(&text)
}

/// Reads a metalink document. Only metalinks of one file are supported.
pub fn parse(xml: &str) -> Result<Metalink> {
    let bad = |e: &dyn std::fmt::Display| Error::State(format!("not a metalink: {e}"));
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut files = 0;
    let mut name = String::new();
    let mut size = None;
    // (algorithm, digest), and (priority, url): lower priority first.
    let mut hashes: Vec<(String, String)> = Vec::new();
    let mut urls: Vec<(u64, String)> = Vec::new();
    let mut in_pieces = false;
    // `<pieces length type>`: the piece length, the algorithm, and the hashes so far.
    let mut piece_length = 0u64;
    let mut piece_type = String::new();
    let mut piece_hashes: Vec<String> = Vec::new();
    // The element whose text is being read, its attribute of interest, and the text so far.
    let mut current: Option<(String, Option<String>)> = None;
    let mut text = String::new();
    loop {
        match reader.read_event().map_err(|e| bad(&e))? {
            Event::Start(e) => {
                let tag = e.local_name().as_ref().to_owned();
                let attr = |key: &str| {
                    e.try_get_attribute(key).ok().flatten().and_then(|a| {
                        a.normalized_value(XmlVersion::Implicit1_0)
                            .ok()
                            .map(|v| v.into_owned())
                    })
                };
                match tag.as_str() {
                    "file" => {
                        files += 1;
                        if files == 1 {
                            name = attr("name").unwrap_or_default();
                        }
                    }
                    "pieces" => {
                        in_pieces = true;
                        if files == 1 {
                            piece_length = attr("length").and_then(|l| l.parse().ok()).unwrap_or(0);
                            piece_type = attr("type")
                                .unwrap_or_default()
                                .to_ascii_lowercase()
                                .replace('-', "");
                        }
                    }
                    "hash" if in_pieces => current = Some(("piece".into(), None)),
                    "hash" => current = Some((tag, attr("type"))),
                    // Metalink 4 ranks by `priority` (1 first), 3 by `preference` (100 first).
                    "url" => {
                        let rank = attr("priority")
                            .and_then(|p| p.parse::<u64>().ok())
                            .or_else(|| {
                                attr("preference")
                                    .and_then(|p| p.parse::<u64>().ok())
                                    .map(|p| 1000u64.saturating_sub(p))
                            })
                            .unwrap_or(u64::MAX);
                        current = Some((tag, Some(rank.to_string())));
                    }
                    "size" => current = Some((tag, None)),
                    _ => {}
                }
                text.clear();
            }
            Event::Text(t) => text.push_str(&t.into_inner()),
            Event::GeneralRef(r) => {
                let entity = r.into_inner();
                text.push_str(match entity.as_ref() {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "apos" => "'",
                    _ => "",
                });
            }
            Event::End(e) => {
                let tag = e.local_name().as_ref().to_owned();
                if tag == "pieces" {
                    in_pieces = false;
                }
                if files <= 1
                    && in_pieces
                    && tag == "hash"
                    && current.as_ref().is_some_and(|(open, _)| open == "piece")
                {
                    current = None;
                    piece_hashes.push(format!("{piece_type}:{}", text.trim()));
                } else if files <= 1
                    && let Some((open, attr)) = current.take()
                    && open == tag
                {
                    let value = text.trim().to_owned();
                    match tag.as_str() {
                        "size" => size = value.parse().ok(),
                        "hash" => {
                            if let Some(kind) = attr {
                                hashes.push((kind.to_ascii_lowercase().replace('-', ""), value));
                            }
                        }
                        "url" => {
                            let rank = attr.and_then(|r| r.parse().ok()).unwrap_or(u64::MAX);
                            let lower = value.to_ascii_lowercase();
                            if lower.starts_with("http://") || lower.starts_with("https://") {
                                urls.push((rank, value));
                            }
                        }
                        _ => {}
                    }
                }
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    match files {
        0 => return Err(bad(&"no file in it")),
        1 => {}
        n => {
            return Err(Error::State(format!(
                "the metalink lists {n} files; only single-file metalinks are supported"
            )));
        }
    }
    urls.sort_by_key(|(rank, _)| *rank);
    let mut ordered: Vec<String> = Vec::new();
    for (_, url) in urls {
        if !ordered.contains(&url) {
            ordered.push(url);
        }
    }
    if ordered.is_empty() {
        return Err(Error::State(
            "the metalink lists no web address for its file".into(),
        ));
    }
    let checksum = ["sha512", "sha256", "sha1", "md5"].iter().find_map(|want| {
        hashes
            .iter()
            .find(|(kind, _)| kind == want)
            .map(|(kind, digest)| format!("{kind}:{digest}"))
    });
    // Usable only when every hash reads as one (a known algorithm, the right length).
    let pieces = (piece_length > 0
        && !piece_hashes.is_empty()
        && piece_hashes
            .iter()
            .all(|h| h.parse::<crate::Checksum>().is_ok()))
    .then_some(crate::pieces::Pieces {
        length: piece_length,
        hashes: piece_hashes,
    });
    Ok(Metalink {
        name: crate::name::sanitize(&name),
        size,
        checksum,
        urls: ordered,
        pieces,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_metalink_4() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <origin dynamic="true">https://download.example.org/a.iso</origin>
  <file name="a.iso">
    <size>414187520</size>
    <hash type="md5">a95be839afc4a9094a0d0b89c240be68</hash>
    <hash type="sha-256">7f9ceb23ab70e4747c6f787ed0f755be05a0e92f3c17d692595162809af8d60a</hash>
    <pieces length="4194304" type="sha-1">
      <hash>a05ee91664b6deb91df466028746a8bd81e065d8</hash>
    </pieces>
    <!-- Mirrors in the same country -->
    <url location="DE" priority="2">https://two.example.org/a.iso?x=1&amp;y=2</url>
    <url location="TR" priority="1">https://one.example.org/a.iso</url>
    <url priority="3">ftp://ftp.example.org/a.iso</url>
    <metaurl mediatype="torrent">https://example.org/a.iso.torrent</metaurl>
  </file>
</metalink>"#;
        let m = parse(xml).expect("parse");
        assert_eq!(m.name, "a.iso");
        assert_eq!(m.size, Some(414_187_520));
        assert_eq!(
            m.checksum.as_deref(),
            Some("sha256:7f9ceb23ab70e4747c6f787ed0f755be05a0e92f3c17d692595162809af8d60a")
        );
        assert_eq!(
            m.urls,
            [
                "https://one.example.org/a.iso",
                "https://two.example.org/a.iso?x=1&y=2"
            ]
        );
        assert!(
            m.checksum
                .clone()
                .expect("sum")
                .parse::<crate::Checksum>()
                .is_ok()
        );
        let pieces = m.pieces.expect("pieces");
        assert_eq!(pieces.length, 4_194_304);
        assert_eq!(
            pieces.hashes,
            ["sha1:a05ee91664b6deb91df466028746a8bd81e065d8"]
        );
    }

    #[test]
    fn reads_metalink_3() {
        let xml = r#"<?xml version="1.0"?>
<metalink version="3.0" xmlns="http://www.metalinker.org/">
  <files>
    <file name="b.tar.gz">
      <size>1024</size>
      <verification>
        <hash type="sha1">4b2bff9711e5f1389bd394adb55f1c5e9a4c22b5</hash>
      </verification>
      <resources>
        <url type="http" preference="50">http://slow.example.org/b.tar.gz</url>
        <url type="http" preference="100">http://fast.example.org/b.tar.gz</url>
      </resources>
    </file>
  </files>
</metalink>"#;
        let m = parse(xml).expect("parse");
        assert_eq!(m.name, "b.tar.gz");
        assert_eq!(m.size, Some(1024));
        assert_eq!(
            m.checksum.as_deref(),
            Some("sha1:4b2bff9711e5f1389bd394adb55f1c5e9a4c22b5")
        );
        assert_eq!(
            m.urls,
            [
                "http://fast.example.org/b.tar.gz",
                "http://slow.example.org/b.tar.gz"
            ]
        );
    }

    #[test]
    fn refuses_what_it_cannot_use() {
        assert!(parse("<html><body>Not found</body></html>").is_err());
        let two = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <file name="a"><url>https://x/a</url></file>
  <file name="b"><url>https://x/b</url></file>
</metalink>"#;
        assert!(
            parse(two)
                .expect_err("two files")
                .to_string()
                .contains("2 files")
        );
        assert!(is_metalink("https://x.org/a.iso.meta4"));
        assert!(is_metalink("https://x.org/a.METALINK?mirrorlist"));
        assert!(!is_metalink("https://x.org/a.iso"));
    }
}
