//! Choosing a file name for a download: from `Content-Disposition`, else the URL path, with an
//! extension from `Content-Type` when the name has none, then made safe to create on Windows.

/// Used when nothing better is known.
pub const FALLBACK: &str = "download";

/// NTFS allows 255 UTF-16 units per name; the `.idma-state` suffix must fit as well.
pub(crate) const MAX_UNITS: usize = 255 - ".idma-state".len();

/// The name the server suggests, or the last segment of `final_url` (after redirects), or of
/// `url`; an extension is added from `content_type` when the name has none. Never empty, never
/// a path, never a reserved device name.
pub fn choose(
    content_disposition: Option<&[u8]>,
    content_type: Option<&str>,
    final_url: &str,
    url: &str,
) -> String {
    let raw = content_disposition
        .and_then(from_content_disposition)
        .or_else(|| from_url(final_url))
        .or_else(|| from_url(url));
    let mut name = sanitize(raw.as_deref().unwrap_or(FALLBACK));
    if !has_extension(&name)
        && let Some(ext) = content_type.and_then(extension_for)
    {
        name = sanitize(&format!("{name}.{ext}"));
    }
    name
}

/// `filename*` (RFC 8187, preferred) or `filename` from a `Content-Disposition` value.
pub fn from_content_disposition(value: &[u8]) -> Option<String> {
    // Servers put raw UTF-8 in `filename` often enough; anything else is Latin-1.
    let value = match std::str::from_utf8(value) {
        Ok(s) => s.to_owned(),
        Err(_) => value.iter().map(|&b| b as char).collect(),
    };
    let mut plain = None;
    let mut extended = None;
    for (key, val) in params(&value) {
        match key.to_ascii_lowercase().as_str() {
            "filename*" => extended = extended.or_else(|| decode_ext_value(&val)),
            "filename" => plain = plain.or(Some(val)),
            _ => {}
        }
    }
    extended.or(plain).filter(|n| !n.trim().is_empty())
}

/// `key=value` parameters after the disposition type; values may be quoted.
fn params(value: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = match value.split_once(';') {
        Some((_, r)) => r,
        None => return out,
    };
    loop {
        rest = rest.trim_start_matches([' ', '\t', ';']);
        let Some((key, after)) = rest.split_once('=') else {
            return out;
        };
        let after = after.trim_start();
        let (val, next) = if let Some(quoted) = after.strip_prefix('"') {
            let mut val = String::new();
            let mut chars = quoted.char_indices();
            let mut end = quoted.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, escaped)) = chars.next() {
                            val.push(escaped);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    c => val.push(c),
                }
            }
            (val, &quoted[end..])
        } else {
            let end = after.find(';').unwrap_or(after.len());
            (after[..end].trim_end().to_owned(), &after[end..])
        };
        out.push((key.trim().to_owned(), val));
        rest = next;
    }
}

/// `charset'language'percent-encoded` (RFC 8187); UTF-8 and ISO-8859-1 are understood.
fn decode_ext_value(v: &str) -> Option<String> {
    let mut parts = v.splitn(3, '\'');
    let charset = parts.next()?.to_ascii_lowercase();
    let _language = parts.next()?;
    let bytes = percent_decode(parts.next()?);
    match charset.as_str() {
        "utf-8" | "utf8" => String::from_utf8(bytes).ok(),
        "iso-8859-1" | "latin1" => Some(bytes.iter().map(|&b| b as char).collect()),
        _ => None,
    }
}

/// The last non-empty path segment of `url`, percent-decoded.
pub fn from_url(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map_or(url, |(_, r)| r);
    let path = after_scheme
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .split_once('/')
        .map(|(_, p)| p)?;
    let segment = path.rsplit('/').find(|s| !s.is_empty())?;
    let bytes = percent_decode(segment);
    let name = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    Some(name).filter(|n| !n.trim().is_empty())
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(hex) = b.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(hex).unwrap_or("zz"), 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// A name that can be created as a single file in any folder on Windows (and elsewhere):
/// only the part after the last `/` or `\`, no characters Windows forbids, no trailing dots
/// or spaces, not a device name such as `CON` or `com1.txt`, and short enough.
pub fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut s: String = base
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    s = s.trim_start().trim_end_matches(['.', ' ']).to_owned();
    if s.is_empty() || s.chars().all(|c| c == '.') {
        return FALLBACK.to_owned();
    }
    if is_reserved(&s) {
        s.insert(0, '_');
    }
    truncate(&s)
}

/// Device names are reserved with any extension, e.g. `nul.tar.gz`.
fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().trim_end();
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((upper.starts_with("COM") || upper.starts_with("LPT"))
        && upper.len() == 4
        && upper.as_bytes()[3].is_ascii_digit())
}

/// Shortens the stem, keeping the extension, to at most [`MAX_UNITS`] UTF-16 units.
fn truncate(name: &str) -> String {
    let units = |s: &str| s.encode_utf16().count();
    if units(name) <= MAX_UNITS {
        return name.to_owned();
    }
    let (stem, ext) = split_extension(name);
    let ext = if units(ext) < MAX_UNITS / 2 { ext } else { "" };
    let mut out = String::new();
    for c in stem.chars() {
        if units(&out) + c.len_utf16() + units(ext) > MAX_UNITS {
            break;
        }
        out.push(c);
    }
    let out = out.trim_end_matches(['.', ' ']);
    format!("{out}{ext}")
}

/// `("archive.tar", ".gz")`; a leading dot (`.bashrc`) is not an extension.
pub fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// A short alphanumeric suffix with a letter in it: `.zip`, `.mp3`, not the `.2` of `tool-1.2`.
fn has_extension(name: &str) -> bool {
    let (_, ext) = split_extension(name);
    (2..=6).contains(&ext.len())
        && ext[1..].chars().all(|c| c.is_ascii_alphanumeric())
        && ext[1..].chars().any(|c| c.is_ascii_alphabetic())
}

/// Extension for the media types downloads usually come as; `None` for generic ones such as
/// `application/octet-stream`.
fn extension_for(content_type: &str) -> Option<&'static str> {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    Some(match mime.as_str() {
        "application/zip" | "application/x-zip-compressed" => "zip",
        "application/x-7z-compressed" => "7z",
        "application/x-rar-compressed" | "application/vnd.rar" => "rar",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/x-tar" => "tar",
        "application/pdf" => "pdf",
        "application/x-msdownload" | "application/vnd.microsoft.portable-executable" => "exe",
        "application/x-msi" | "application/x-ms-installer" => "msi",
        "application/x-iso9660-image" => "iso",
        "application/json" => "json",
        "text/plain" => "txt",
        "text/html" => "html",
        "text/csv" => "csv",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "audio/ogg" => "ogg",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/x-matroska" => "mkv",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cd(v: &str) -> Option<String> {
        from_content_disposition(v.as_bytes())
    }

    #[test]
    fn content_disposition() {
        assert_eq!(cd("attachment; filename=a.zip").as_deref(), Some("a.zip"));
        assert_eq!(
            cd(r#"attachment; filename="my \"file\".zip""#).as_deref(),
            Some(r#"my "file".zip"#)
        );
        assert_eq!(
            cd("attachment; filename=\"_ay.zip\"; filename*=UTF-8''%C3%A7ay.zip").as_deref(),
            Some("çay.zip")
        );
        // `filename*` wins even when it comes first.
        assert_eq!(
            cd("attachment; filename*=utf-8'tr'%C5%9Fey.pdf; filename=x.pdf").as_deref(),
            Some("şey.pdf")
        );
        assert_eq!(
            cd("attachment; filename*=iso-8859-1''caf%E9.txt").as_deref(),
            Some("café.txt")
        );
        assert_eq!(
            cd("attachment; filename=\"a;b.zip\"").as_deref(),
            Some("a;b.zip")
        );
        assert_eq!(cd("inline; FILENAME=Up.ZIP").as_deref(), Some("Up.ZIP"));
        // Raw UTF-8 bytes.
        assert_eq!(
            from_content_disposition("attachment; filename=\"ğüş.txt\"".as_bytes()).as_deref(),
            Some("ğüş.txt")
        );
        assert_eq!(cd("attachment"), None);
        assert_eq!(cd("attachment; filename=\"\""), None);
    }

    #[test]
    fn url_segments() {
        assert_eq!(
            from_url("https://h.com/dir/file.iso?x=1#f").as_deref(),
            Some("file.iso")
        );
        assert_eq!(from_url("https://h.com/dir/").as_deref(), Some("dir"));
        assert_eq!(from_url("https://h.com/"), None);
        assert_eq!(from_url("https://h.com"), None);
        assert_eq!(
            from_url("http://h/%C3%A7ay%20bardak.mp4").as_deref(),
            Some("çay bardak.mp4")
        );
        assert_eq!(from_url("http://h/a%2Fb").as_deref(), Some("a/b"));
    }

    #[test]
    fn sanitizing() {
        assert_eq!(sanitize("../../evil.exe"), "evil.exe");
        assert_eq!(sanitize("..\\..\\evil.exe"), "evil.exe");
        assert_eq!(sanitize("a/b"), "b");
        assert_eq!(sanitize("what?<>:*|\".txt"), "what_______.txt");
        assert_eq!(sanitize("tab\there"), "tab_here");
        assert_eq!(sanitize("name. . "), "name");
        assert_eq!(sanitize(".."), FALLBACK);
        assert_eq!(sanitize("   "), FALLBACK);
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("con.txt"), "_con.txt");
        assert_eq!(sanitize("nul.tar.gz"), "_nul.tar.gz");
        assert_eq!(sanitize("COM1.log"), "_COM1.log");
        assert_eq!(sanitize("COM10.log"), "COM10.log");
        assert_eq!(sanitize("console.txt"), "console.txt");
        assert_eq!(sanitize(".bashrc"), ".bashrc");

        let long = format!("{}.zip", "ş".repeat(400));
        let short = sanitize(&long);
        assert!(short.ends_with(".zip"));
        assert_eq!(short.encode_utf16().count(), MAX_UNITS);
    }

    #[test]
    fn choosing() {
        let page = "https://h.com/get?id=7";
        assert_eq!(choose(None, None, page, page), "get");
        assert_eq!(choose(None, Some("application/zip"), page, page), "get.zip");
        assert_eq!(
            choose(None, Some("application/octet-stream"), page, page),
            "get"
        );
        assert_eq!(
            choose(
                None,
                Some("application/pdf; charset=binary"),
                "https://h/a.pdf",
                page
            ),
            "a.pdf"
        );
        assert_eq!(
            choose(
                Some(b"attachment; filename=\"../CON\""),
                Some("text/plain"),
                page,
                page
            ),
            "_CON.txt"
        );
        // Redirect target without a path: fall back to the original URL.
        assert_eq!(
            choose(None, None, "https://cdn.com/", "https://h.com/setup.exe"),
            "setup.exe"
        );
        assert_eq!(
            choose(None, None, "https://h.com/", "https://h.com/"),
            FALLBACK
        );
        assert_eq!(
            choose(None, Some("text/html"), "https://h.com/", "https://h.com/"),
            "download.html"
        );
        // Version numbers are not extensions.
        assert_eq!(
            choose(None, Some("application/zip"), "https://h/tool-1.2", page),
            "tool-1.2.zip"
        );
    }
}
