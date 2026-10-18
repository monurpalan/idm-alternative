//! Site grabber: reads a page and the pages it links to, down to a chosen depth, and
//! collects the files they link to (archives, documents, video, pictures…) for the user to
//! pick from. Pages are told from files by their address alone, so nothing is downloaded
//! to find out; only pages are fetched.

use std::collections::{HashSet, VecDeque};

use shardload_engine::{CancellationToken, Options};
use url::Url;

/// Pages read at the same time.
const PARALLEL: usize = 4;
/// The most of a page that is read.
const PAGE_LIMIT: usize = 4 << 20;
/// Link texts are cut to this many characters.
const TEXT_LIMIT: usize = 200;

/// What to grab.
#[derive(Debug, Clone)]
pub struct GrabOptions {
    /// 1: the files the start page links to; 2: also those of the pages it links to; and
    /// so on.
    pub depth: u32,
    /// Follow only pages on the start page's site (its host, with or without `www.`).
    /// Files are collected from anywhere.
    pub same_site: bool,
    /// The most pages read in all.
    pub max_pages: usize,
}

impl Default for GrabOptions {
    fn default() -> Self {
        Self {
            depth: 1,
            same_site: true,
            max_pages: 200,
        }
    }
}

/// A file found, with the text of the link to it (empty for pictures and the like).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Found {
    pub url: String,
    pub text: String,
    /// The page it was found on, sent as the referrer when it is downloaded.
    pub page: String,
}

/// How far a grab has got.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct GrabProgress {
    pub pages: usize,
    pub files: usize,
}

/// Grabs the files linked from `start` and, to `opts.depth`, from the pages it links to.
/// Pages that cannot be read are skipped; only the start page failing is an error.
pub async fn grab(
    start: &str,
    opts: &GrabOptions,
    headers: &[(String, String)],
    engine: &Options,
    progress: impl Fn(GrabProgress),
    cancel: &CancellationToken,
) -> Result<Vec<Found>, String> {
    let start = Url::parse(start.trim()).map_err(|e| format!("not a web address: {e}"))?;
    let site = site_of(&start);
    let mut seen_pages: HashSet<String> = HashSet::from([strip_fragment(&start)]);
    let mut seen_files: HashSet<String> = HashSet::new();
    let mut found: Vec<Found> = Vec::new();
    let mut level: VecDeque<Url> = VecDeque::from([start.clone()]);
    let mut pages_read = 0;
    for depth in 1..=opts.depth.max(1) {
        let mut next: VecDeque<Url> = VecDeque::new();
        while !level.is_empty() && pages_read < opts.max_pages {
            let take = PARALLEL.min(level.len()).min(opts.max_pages - pages_read);
            let batch: Vec<Url> = level.drain(..take).collect();
            let reads = batch.iter().map(|page| async move {
                let r =
                    shardload_engine::fetch_text(page.as_str(), headers, engine, PAGE_LIMIT).await;
                (page, r)
            });
            let results = tokio::select! {
                r = futures_util::future::join_all(reads) => r,
                () = cancel.cancelled() => return Err("cancelled".into()),
            };
            for (page, result) in results {
                pages_read += 1;
                let (html, final_url) = match result {
                    Ok(r) => r,
                    Err(e) if page == &start => return Err(e.to_string()),
                    Err(e) => {
                        tracing::debug!(page = %page, "page not read: {e}");
                        continue;
                    }
                };
                let base = Url::parse(&final_url).unwrap_or_else(|_| page.clone());
                for link in links(&html, &base) {
                    let key = strip_fragment(&link.url);
                    if is_page(&link.url) {
                        if depth < opts.depth
                            && (!opts.same_site || site_of(&link.url) == site)
                            && seen_pages.insert(key)
                        {
                            next.push_back(link.url);
                        }
                    } else if seen_files.insert(key.clone()) {
                        found.push(Found {
                            url: key,
                            text: link.text,
                            page: base.to_string(),
                        });
                    }
                }
                progress(GrabProgress {
                    pages: pages_read,
                    files: found.len(),
                });
            }
        }
        level = next;
        if level.is_empty() || pages_read >= opts.max_pages {
            break;
        }
    }
    Ok(found)
}

/// A link in a page.
#[derive(Debug, PartialEq, Eq)]
struct Link {
    url: Url,
    text: String,
}

/// The links of a page: `<a href>` with their text, and the sources of pictures, video,
/// audio and frames. Links that are not web addresses (`mailto:`, `javascript:`, …) are
/// left out.
fn links(html: &str, page: &Url) -> Vec<Link> {
    let mut out = Vec::new();
    let mut base = page.clone();
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        rest = &rest[lt + 1..];
        // Comments, and the contents of scripts and styles, hold no links to follow.
        if let Some(after) = rest.strip_prefix("!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        let name_end = rest
            .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len());
        let name = rest[..name_end].to_ascii_lowercase();
        let tag_end = tag_end(rest);
        let attrs = attributes(&rest[name_end..tag_end]);
        rest = rest.get(tag_end + 1..).unwrap_or("");
        let attr = |key: &str| {
            attrs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        match name.as_str() {
            "script" | "style" => {
                let close = format!("</{name}");
                rest = find_ascii_ci(rest, &close).map_or("", |i| &rest[i..]);
            }
            "base" => {
                if let Some(u) = attr("href").and_then(|h| page.join(h).ok()) {
                    base = u;
                }
            }
            "a" | "area" => {
                if let Some(u) = attr("href").and_then(|h| resolve(&base, h)) {
                    // The text up to the closing tag, tags taken out.
                    let text_end = find_ascii_ci(rest, "</a").unwrap_or(0);
                    let text = if name == "a" {
                        clean_text(&rest[..text_end])
                    } else {
                        attr("alt").map(clean_text).unwrap_or_default()
                    };
                    out.push(Link { url: u, text });
                }
            }
            "img" | "video" | "audio" | "source" | "embed" | "iframe" | "frame" => {
                for key in ["src", "data-src"] {
                    if let Some(u) = attr(key).and_then(|h| resolve(&base, h)) {
                        let text = attr("alt")
                            .or(attr("title"))
                            .map(clean_text)
                            .unwrap_or_default();
                        out.push(Link { url: u, text });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Where a tag that starts at `s` (after its `<`) ends: its `>`, outside quotes.
fn tag_end(s: &str) -> usize {
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return i,
            _ => {}
        }
    }
    s.len()
}

/// `name=value` pairs of a tag (names lowercased, values decoded).
fn attributes(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        let key = s[start..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            let value = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                let v0 = i + 1;
                i = v0;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                let v = &s[v0..i];
                i += 1;
                v
            } else {
                let v0 = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                &s[v0..i]
            };
            if !key.is_empty() {
                out.push((key, decode_entities(value)));
            }
        } else if !key.is_empty() {
            out.push((key, String::new()));
        } else {
            i += 1;
        }
    }
    out
}

fn find_ascii_ci(hay: &str, needle: &str) -> Option<usize> {
    let n = needle.len();
    hay.as_bytes()
        .windows(n)
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// A link's address against the page's, if it is a web address at all.
fn resolve(base: &Url, href: &str) -> Option<Url> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let u = base.join(href).ok()?;
    matches!(u.scheme(), "http" | "https" | "ftp").then_some(u)
}

/// Text as a person reads it: tags out, entities decoded, spaces collapsed, cut short.
fn clean_text(s: &str) -> String {
    let mut plain = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                plain.push(' ');
            }
            c if !in_tag => plain.push(c),
            _ => {}
        }
    }
    let text = decode_entities(&plain);
    let words: Vec<&str> = text.split_whitespace().collect();
    words.join(" ").chars().take(TEXT_LIMIT).collect()
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Whether an address looks like a page rather than a file: no extension, or one pages
/// have. Query strings do not count (`download.php?id=3` is a page; servers that answer
/// it with a file are not followed).
fn is_page(u: &Url) -> bool {
    let path = u.path();
    let last = path.rsplit('/').next().unwrap_or_default();
    match last.rsplit_once('.') {
        None => true,
        Some((_, ext)) => matches!(
            ext.to_ascii_lowercase().as_str(),
            "html"
                | "htm"
                | "xhtml"
                | "shtml"
                | "php"
                | "asp"
                | "aspx"
                | "jsp"
                | "cgi"
                | "pl"
                // MirrorBrain's page of a file's mirrors (LibreOffice and others).
                | "mirrorlist"
        ),
    }
}

/// The site a page belongs to: its host without `www.`.
fn site_of(u: &Url) -> String {
    let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
    host.strip_prefix("www.").map(str::to_owned).unwrap_or(host)
}

fn strip_fragment(u: &Url) -> String {
    let mut u = u.clone();
    u.set_fragment(None);
    u.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> Url {
        Url::parse("https://www.example.org/files/index.html").expect("url")
    }

    #[test]
    fn finds_links_pictures_and_media() {
        let html = r##"<html><head><title>x</title>
<script>var s = "<a href='/not-a-link.zip'>";</script>
<style>a { background: url(/bg.png) }</style></head><body>
<!-- <a href="/commented.zip">old</a> -->
<a href="setup.exe" class=btn>Download <b>Setup</b>&nbsp;(64&#8209;bit)</a>
<A HREF='/docs/manual.pdf'>Manual</A>
<a href=../other/page.html>Other page</a>
<a href="#top">Top</a> <a href="mailto:a@b.c">Mail</a> <a href="javascript:void(0)">x</a>
<img src="pics/photo%201.jpg" alt="A photo">
<video><source src="https://cdn.example.net/clip.mp4" type="video/mp4"></video>
<a href="get.php?id=3&amp;mirror=2">Get</a>
</body></html>"##;
        let got: Vec<(String, String)> = links(html, &page())
            .into_iter()
            .map(|l| (l.url.to_string(), l.text))
            .collect();
        assert_eq!(
            got,
            [
                (
                    "https://www.example.org/files/setup.exe".to_owned(),
                    "Download Setup (64\u{2011}bit)".to_owned()
                ),
                (
                    "https://www.example.org/docs/manual.pdf".into(),
                    "Manual".into()
                ),
                (
                    "https://www.example.org/other/page.html".into(),
                    "Other page".into()
                ),
                (
                    "https://www.example.org/files/pics/photo%201.jpg".into(),
                    "A photo".into()
                ),
                ("https://cdn.example.net/clip.mp4".into(), String::new()),
                (
                    "https://www.example.org/files/get.php?id=3&mirror=2".into(),
                    "Get".into()
                ),
            ]
        );
    }

    #[test]
    fn base_href_changes_where_links_point() {
        let html = r#"<base href="https://mirror.example.com/pub/"><a href="a.zip">a</a>"#;
        let got = links(html, &page());
        assert_eq!(got[0].url.as_str(), "https://mirror.example.com/pub/a.zip");
    }

    #[test]
    fn tells_pages_from_files() {
        let u = |s: &str| Url::parse(s).expect("url");
        assert!(is_page(&u("https://x.org/")));
        assert!(is_page(&u("https://x.org/downloads")));
        assert!(is_page(&u("https://x.org/a/b.html")));
        assert!(is_page(&u("https://x.org/get.php?id=1")));
        assert!(!is_page(&u("https://x.org/a.zip")));
        assert!(!is_page(&u("https://x.org/a.tar.gz?x=1")));
        assert!(is_page(&u("https://x.org/a.msi.mirrorlist")));
        assert_eq!(site_of(&u("https://WWW.x.org/a")), "x.org");
    }
}
