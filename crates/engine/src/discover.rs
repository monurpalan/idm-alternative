//! Finding a checksum for a download the user gave none for: the digest GitHub keeps for a
//! release asset, or a checksum file next to the download (`<file>.sha256`, `SHA256SUMS`…).
//!
//! All of it is best effort and runs while the file downloads: an error, a slow server or a
//! file that is not what it seems just means no checksum. It is also meant to be polite: a
//! few requests at a time in order of likelihood, none after the first answer, none more
//! once a server turns us away, and what a folder has (or lacks) is remembered for a while,
//! so a queue of files from one folder does not ask the same things again.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::Deserialize;

use crate::checksum::Checksum;
use crate::remote::{Fetched, Http, HttpConfig};

/// The longest one request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest the whole search may take.
const TIMEOUT: Duration = Duration::from_secs(15);
/// Requests in flight at once.
const IN_FLIGHT: usize = 3;
/// Checksum files (and API answers) larger than this are not read.
const MAX_SUM_FILE: usize = 1 << 20;
/// How long what was learned about a folder or a release is trusted.
const REMEMBER: Duration = Duration::from_secs(600);

/// A place a checksum may be published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Candidate {
    /// The download's own checksum file: its name plus this suffix.
    Own(&'static str),
    /// A list of the folder's files.
    List(&'static str),
}

/// In the order they are asked for: the most common first.
const CANDIDATES: [Candidate; 13] = [
    Candidate::Own(".sha256"),
    Candidate::Own(".sha512"),
    Candidate::List("SHA256SUMS"),
    Candidate::Own(".sha256sum"),
    Candidate::Own(".sha512sum"),
    Candidate::List("SHA256SUMS.txt"),
    Candidate::List("sha256sums.txt"),
    Candidate::List("sha256sum.txt"),
    Candidate::List("SHA512SUMS"),
    Candidate::List("CHECKSUMS"),
    Candidate::List("checksums.txt"),
    Candidate::Own(".sha1"),
    Candidate::Own(".md5"),
];

/// What earlier searches in this process learned.
#[derive(Default)]
struct Memory {
    folders: HashMap<String, (Instant, Folder)>,
    /// API answers per release; `None` when there was none.
    releases: HashMap<String, (Instant, Option<Arc<str>>)>,
    /// The API refused (rate limit) at this moment.
    github_refused: Option<Instant>,
}

#[derive(Default, Clone)]
struct Folder {
    /// Lists asked for: their text, or `None` when there is none.
    lists: HashMap<&'static str, Option<Arc<str>>>,
    /// Whether files here come with checksum files of their own; `None` until known.
    own_sums: Option<bool>,
    /// The server turned us away or could not be reached.
    refused: bool,
}

static MEMORY: LazyLock<Mutex<Memory>> = LazyLock::new(Mutex::default);

fn memory() -> std::sync::MutexGuard<'static, Memory> {
    MEMORY.lock().unwrap_or_else(PoisonError::into_inner)
}

fn fresh(at: Instant) -> bool {
    at.elapsed() < REMEMBER
}

/// What is known about `folder`, forgotten once stale.
fn folder_memory(folder: &str) -> Folder {
    match memory().folders.get(folder) {
        Some((at, f)) if fresh(*at) => f.clone(),
        _ => Folder::default(),
    }
}

/// Changes what is known about `folder` (starting afresh when stale).
fn remember(folder: &str, change: impl FnOnce(&mut Folder)) {
    let mut m = memory();
    let entry = m
        .folders
        .entry(folder.to_owned())
        .or_insert_with(|| (Instant::now(), Folder::default()));
    if !fresh(entry.0) {
        *entry = (Instant::now(), Folder::default());
    }
    change(&mut entry.1);
}
/// Where a checksum from GitHub's own digest of a release asset is said to come from.
const GITHUB_RELEASE: &str = "GitHub release";

/// A checksum found for a download, and where it was found: the checksum file's name (e.g.
/// `SHA256SUMS`) or [`GITHUB_RELEASE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Found {
    pub checksum: Checksum,
    pub from: String,
}

/// Looks for a published checksum of the file at `url` (http or https), listed under one of
/// `names`. `site` sends the download's own headers and is used only for the download's
/// own folder; `cfg` (without those headers) is for asking GitHub.
pub(crate) async fn discover(
    site: Http,
    cfg: HttpConfig,
    url: String,
    names: Vec<String>,
) -> Option<Found> {
    let scheme = url.split_once("://")?.0.to_ascii_lowercase();
    if (scheme != "http" && scheme != "https") || names.is_empty() {
        return None;
    }
    let search = async {
        if let Some(release) = github_release(&url)
            && let Some(found) = from_github(cfg, &release).await
        {
            return Some(found);
        }
        from_folder(&site, &url, &names).await
    };
    let found = tokio::time::timeout(TIMEOUT, search).await.ok().flatten();
    match &found {
        Some(f) => tracing::info!(from = %f.from, checksum = %f.checksum, "found a checksum"),
        None => tracing::debug!(%url, "no published checksum found"),
    }
    found
}

/// One request; a timeout counts as refused.
async fn fetch(http: &Http, url: &str) -> Fetched {
    tokio::time::timeout(REQUEST_TIMEOUT, http.small_text(url, MAX_SUM_FILE))
        .await
        .unwrap_or(Fetched::Refused)
}

/// Asks the checksum files that may sit next to the download in [`CANDIDATES`] order, a
/// few at a time, and stops at the first that lists the file or once the server turns us
/// away. Lists already read in this folder are not asked for again, and neither are files'
/// own checksum files where an earlier file of the folder had none.
async fn from_folder(http: &Http, url: &str, names: &[String]) -> Option<Found> {
    let (folder, segment) = folder_of(url)?;
    let known = folder_memory(&folder);
    if known.refused {
        return None;
    }
    let own_name = crate::name::from_url(url).unwrap_or_else(|| names[0].clone());
    let candidates = CANDIDATES
        .into_iter()
        .filter(|c| !matches!(c, Candidate::Own(_)) || known.own_sums != Some(false));
    let mut answers = futures_util::stream::iter(candidates)
        .map(|c| {
            let known = &known;
            let folder = &folder;
            let segment = &segment;
            async move {
                let fetched = match c {
                    Candidate::List(name) => match known.lists.get(name) {
                        Some(Some(text)) => Fetched::Text(text.to_string()),
                        Some(None) => Fetched::Absent,
                        None => fetch(http, &format!("{folder}{name}")).await,
                    },
                    Candidate::Own(suffix) => {
                        fetch(http, &format!("{folder}{segment}{suffix}")).await
                    }
                };
                (c, fetched)
            }
        })
        .buffered(IN_FLIGHT);
    let mut own_seen = None;
    let mut found = None;
    while let Some((c, fetched)) = answers.next().await {
        let text = match fetched {
            Fetched::Refused => {
                tracing::debug!(%folder, "the server refused a checksum file; asking no more");
                remember(&folder, |f| f.refused = true);
                break;
            }
            Fetched::Absent => None,
            Fetched::Text(text) => Some(text),
        };
        match c {
            Candidate::List(name) => {
                if !known.lists.contains_key(name) {
                    let text = text.as_deref().map(Arc::from);
                    remember(&folder, |f| {
                        f.lists.insert(name, text);
                    });
                }
            }
            Candidate::Own(_) => {
                own_seen = Some(own_seen.unwrap_or(false) || text.is_some());
            }
        }
        let own = matches!(c, Candidate::Own(_));
        if let Some(checksum) = text.and_then(|t| find_in_sum_file(&t, names, own)) {
            let from = match c {
                Candidate::Own(suffix) => format!("{own_name}{suffix}"),
                Candidate::List(name) => name.to_owned(),
            };
            found = Some(Found { checksum, from });
            break;
        }
    }
    if let Some(seen) = own_seen
        && known.own_sums != Some(true)
    {
        remember(&folder, |f| {
            f.own_sums = Some(f.own_sums == Some(true) || seen)
        });
    }
    found
}

/// The URL of the folder holding `url`'s file (ending in `/`, without query or fragment),
/// and the file's last path segment as written in the URL.
fn folder_of(url: &str) -> Option<(String, String)> {
    let mut u = url::Url::parse(url).ok()?;
    u.set_query(None);
    u.set_fragment(None);
    let path = u.path().to_owned();
    let slash = path.rfind('/')?;
    let segment = &path[slash + 1..];
    if segment.is_empty() {
        return None;
    }
    u.set_path(&path[..=slash]);
    Some((u.to_string(), segment.to_owned()))
}

/// The checksum a checksum file gives for one of `names`. Understands GNU coreutils lines
/// (`<hex>  name`, `<hex> *name`) and BSD ones (`SHA256 (name) = <hex>`). `own` is a file
/// of the download alone (`<file>.sha256`): a bare digest counts there, and so does its
/// only entry when that names the file differently (it may have been renamed on upload).
pub(crate) fn find_in_sum_file(text: &str, names: &[String], own: bool) -> Option<Checksum> {
    let mut entries = 0;
    let mut only = None;
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((checksum, name)) = bsd_line(line).or_else(|| gnu_line(line)) else {
            continue;
        };
        if let Some(name) = &name
            && listed_as(name, names)
        {
            return Some(checksum);
        }
        entries += 1;
        only = Some(checksum);
    }
    (own && entries == 1).then_some(only).flatten()
}

/// `<hex>  name`, `<hex> *name` (binary mode) or a bare `<hex>`. A leading `\` marks a name
/// with escaped backslashes or newlines.
fn gnu_line(line: &str) -> Option<(Checksum, Option<String>)> {
    let (escaped, line) = match line.strip_prefix('\\') {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let (digest, rest) = match line.split_once([' ', '\t']) {
        Some((d, r)) => (d, r),
        None => (line, ""),
    };
    if !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let checksum: Checksum = digest.parse().ok()?;
    let rest = rest.strip_prefix([' ', '\t']).unwrap_or(rest);
    let name = rest.strip_prefix('*').unwrap_or(rest);
    if name.trim().is_empty() {
        return Some((checksum, None));
    }
    let name = if escaped {
        unescape(name)
    } else {
        name.to_owned()
    };
    Some((checksum, Some(name)))
}

/// Undoes coreutils' escaping of `\` and newlines in names.
fn unescape(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut chars = name.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some('\\')) => {
                out.push('\\');
                chars.next();
            }
            ('\\', Some('n')) => {
                out.push('\n');
                chars.next();
            }
            (c, _) => out.push(c),
        }
    }
    out
}

/// `SHA256 (name) = <hex>`, also `SHA2-256(name)= <hex>` as OpenSSL writes it.
fn bsd_line(line: &str) -> Option<(Checksum, Option<String>)> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close < open {
        return None;
    }
    let algorithm = match line[..open]
        .trim()
        .to_ascii_lowercase()
        .replace('-', "")
        .as_str()
    {
        "md5" => "md5",
        "sha1" => "sha1",
        "sha256" | "sha2256" => "sha256",
        "sha512" | "sha2512" => "sha512",
        _ => return None,
    };
    let digest = line[close + 1..].trim_start().strip_prefix('=')?.trim();
    let checksum = format!("{algorithm}:{digest}").parse().ok()?;
    Some((checksum, Some(line[open + 1..close].to_owned())))
}

/// Whether a name in a checksum file is one of `names`: as is, without a leading `./`, or
/// by its last path component.
fn listed_as(listed: &str, names: &[String]) -> bool {
    let listed = listed.trim_end();
    let listed = listed.strip_prefix("./").unwrap_or(listed);
    let base = listed.rsplit(['/', '\\']).next().unwrap_or(listed);
    names.iter().any(|n| n == listed || n == base)
}

/// A GitHub release asset: `https://github.com/<owner>/<repo>/releases/download/<tag>/<asset>`
/// (or `…/releases/latest/download/<asset>`).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Release {
    pub owner: String,
    pub repo: String,
    /// As written in the URL (percent-encoded); `None` for the latest release.
    pub tag: Option<String>,
    /// Decoded.
    pub asset: String,
}

impl Release {
    fn api_url(&self) -> String {
        let base = format!(
            "https://api.github.com/repos/{}/{}/releases",
            self.owner, self.repo
        );
        match &self.tag {
            Some(tag) => format!("{base}/tags/{tag}"),
            None => format!("{base}/latest"),
        }
    }
}

pub(crate) fn github_release(url: &str) -> Option<Release> {
    let u = url::Url::parse(url).ok()?;
    if u.scheme() != "https" && u.scheme() != "http" {
        return None;
    }
    let host = u.host_str()?.to_ascii_lowercase();
    if host != "github.com" && host != "www.github.com" {
        return None;
    }
    let segments: Vec<&str> = u.path_segments()?.collect();
    let (owner, repo, tag, asset) = match segments.as_slice() {
        [owner, repo, "releases", "download", tag, asset] => (owner, repo, Some(*tag), asset),
        [owner, repo, "releases", "latest", "download", asset] => (owner, repo, None, asset),
        _ => return None,
    };
    if [owner, repo, asset].iter().any(|s| s.is_empty()) || tag.is_some_and(str::is_empty) {
        return None;
    }
    Some(Release {
        owner: (*owner).to_owned(),
        repo: (*repo).to_owned(),
        tag: tag.map(str::to_owned),
        asset: crate::name::from_url(&format!("x://h/{asset}"))?,
    })
}

#[derive(Deserialize)]
struct ApiRelease {
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    /// `sha256:<hex>`; missing for assets uploaded before GitHub kept digests.
    digest: Option<String>,
}

/// The digest GitHub's API gives for the release's asset. One request per release while
/// its answer is remembered; none at all for a while after the API refused (its limit
/// without a token is 60 requests an hour), and never a retry.
async fn from_github(cfg: HttpConfig, release: &Release) -> Option<Found> {
    let api = release.api_url();
    let known = {
        let m = memory();
        if m.github_refused.is_some_and(fresh) {
            return None;
        }
        m.releases
            .get(&api)
            .filter(|(at, _)| fresh(*at))
            .map(|(_, json)| json.clone())
    };
    let json = match known {
        Some(json) => json?,
        None => {
            let cfg = HttpConfig {
                headers: vec![("Accept".into(), "application/vnd.github+json".into())],
                ..cfg
            };
            let http = Http::new(&cfg).ok()?;
            let json = match fetch(&http, &api).await {
                Fetched::Text(json) => Some(Arc::<str>::from(json)),
                Fetched::Absent => None,
                Fetched::Refused => {
                    tracing::debug!("the GitHub API refused; not asking it for a while");
                    memory().github_refused = Some(Instant::now());
                    return None;
                }
            };
            memory()
                .releases
                .insert(api, (Instant::now(), json.clone()));
            json?
        }
    };
    let checksum = digest_of(&json, &release.asset)?;
    Some(Found {
        checksum,
        from: GITHUB_RELEASE.to_owned(),
    })
}

/// The digest an API answer about a release gives for `asset`.
pub(crate) fn digest_of(json: &str, asset: &str) -> Option<Checksum> {
    let release: ApiRelease = serde_json::from_str(json).ok()?;
    release
        .assets
        .into_iter()
        .find(|a| a.name == asset)?
        .digest?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    const B: &str = "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae";

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| (*s).to_owned()).collect()
    }

    fn sha256(hex: &str) -> Option<Checksum> {
        Some(format!("sha256:{hex}").parse().expect("checksum"))
    }

    #[test]
    fn gnu_lists() {
        let list = format!("{B}  other.iso\n{A}  file.iso\n");
        assert_eq!(
            find_in_sum_file(&list, &names(&["file.iso"]), false),
            sha256(A)
        );
        // Binary mode, CRLF, a BOM, comments and a `./` path.
        let list = format!("\u{feff}# made by sha256sum\r\n{B} *other.iso\r\n{A} *./file.iso\r\n");
        assert_eq!(
            find_in_sum_file(&list, &names(&["file.iso"]), false),
            sha256(A)
        );
        // A file in a subfolder, listed with its path.
        let list = format!("{B}  a/other.iso\n{A}  ./dist/file.iso\n");
        assert_eq!(
            find_in_sum_file(&list, &names(&["file.iso"]), false),
            sha256(A)
        );
        // Escaped names.
        let list = format!("\\{A}  dir\\\\file.iso\n");
        assert_eq!(
            find_in_sum_file(&list, &names(&["dir\\file.iso"]), false),
            sha256(A)
        );
        // Not listed.
        let list = format!("{B}  other.iso\n{A}  third.iso\n");
        assert_eq!(find_in_sum_file(&list, &names(&["file.iso"]), false), None);
        // The second name (the file as saved) matches.
        assert_eq!(
            find_in_sum_file(&list, &names(&["file.iso", "third.iso"]), false),
            sha256(A)
        );
        // Other digest lengths.
        let md5 = "098f6bcd4621d373cade4e832627b4f6";
        assert_eq!(
            find_in_sum_file(&format!("{md5}  f.txt"), &names(&["f.txt"]), false),
            Some(format!("md5:{md5}").parse().expect("md5"))
        );
    }

    #[test]
    fn bsd_lists() {
        let list = format!("SHA256 (other.iso) = {B}\nSHA256 (file.iso) = {A}\n");
        assert_eq!(
            find_in_sum_file(&list, &names(&["file.iso"]), false),
            sha256(A)
        );
        let list = format!("SHA2-256(file.iso)= {A}\r\n");
        assert_eq!(
            find_in_sum_file(&list, &names(&["file.iso"]), false),
            sha256(A)
        );
        // A name with parentheses.
        let list = format!("SHA256 (file (1).iso) = {A}\n");
        assert_eq!(
            find_in_sum_file(&list, &names(&["file (1).iso"]), false),
            sha256(A)
        );
        // An algorithm the checker does not know, or a digest of the wrong length.
        let list = format!("BLAKE2b (file.iso) = {A}\nSHA512 (file.iso) = {A}\n");
        assert_eq!(find_in_sum_file(&list, &names(&["file.iso"]), false), None);
    }

    #[test]
    fn files_of_one_download() {
        // A bare digest.
        assert_eq!(
            find_in_sum_file(&format!("{A}\n"), &names(&["file.iso"]), true),
            sha256(A)
        );
        // The only entry, under the name the file was built with.
        assert_eq!(
            find_in_sum_file(
                &format!("{A}  build-123.iso\n"),
                &names(&["file.iso"]),
                true
            ),
            sha256(A)
        );
        // Neither counts in a folder's list.
        assert_eq!(
            find_in_sum_file(&format!("{A}\n"), &names(&["file.iso"]), false),
            None
        );
        // Several entries, none ours.
        assert_eq!(
            find_in_sum_file(&format!("{A}  a\n{B}  b\n"), &names(&["file.iso"]), true),
            None
        );
    }

    #[test]
    fn not_checksum_files() {
        let html = "<!DOCTYPE html>\n<html><body>Not found</body></html>\n";
        assert_eq!(find_in_sum_file(html, &names(&["file.iso"]), true), None);
        assert_eq!(find_in_sum_file("", &names(&["file.iso"]), true), None);
        let signed = format!(
            "-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\n{A}  file.iso\n-----BEGIN PGP SIGNATURE-----\nabc\n"
        );
        assert_eq!(
            find_in_sum_file(&signed, &names(&["file.iso"]), false),
            sha256(A)
        );
        // Not hex, or no known length.
        assert_eq!(
            find_in_sum_file(
                "zzzz  file.iso\nabcd  file.iso",
                &names(&["file.iso"]),
                true
            ),
            None
        );
    }

    #[test]
    fn folders() {
        assert_eq!(
            folder_of("https://example.com/pub/v1/file%20a.iso?token=x#y"),
            Some((
                "https://example.com/pub/v1/".to_owned(),
                "file%20a.iso".to_owned()
            ))
        );
        assert_eq!(folder_of("https://example.com/pub/"), None);
        assert_eq!(folder_of("not a url"), None);
    }

    #[test]
    fn github_urls() {
        assert_eq!(
            github_release(
                "https://github.com/cli/cli/releases/download/v2.60.0/gh_2.60.0_windows_amd64.zip"
            ),
            Some(Release {
                owner: "cli".into(),
                repo: "cli".into(),
                tag: Some("v2.60.0".into()),
                asset: "gh_2.60.0_windows_amd64.zip".into(),
            })
        );
        let r = github_release("https://github.com/o/r/releases/download/v1%2Bx/my%20app.exe")
            .expect("release");
        assert_eq!(r.tag.as_deref(), Some("v1%2Bx"));
        assert_eq!(r.asset, "my app.exe");
        assert_eq!(
            r.api_url(),
            "https://api.github.com/repos/o/r/releases/tags/v1%2Bx"
        );
        let latest = github_release("https://github.com/o/r/releases/latest/download/a.zip")
            .expect("latest");
        assert_eq!(latest.tag, None);
        assert_eq!(
            latest.api_url(),
            "https://api.github.com/repos/o/r/releases/latest"
        );
        for not in [
            "https://github.com/o/r/archive/refs/tags/v1.zip",
            "https://github.com/o/r/releases/tag/v1",
            "https://example.com/o/r/releases/download/v1/a.zip",
            "https://github.com/o/r/releases/download/v1/",
            "ftp://github.com/o/r/releases/download/v1/a.zip",
        ] {
            assert_eq!(github_release(not), None, "{not}");
        }
    }

    #[test]
    fn github_digests() {
        let json = format!(
            r#"{{"tag_name":"v1","assets":[
                {{"name":"a.zip","digest":"sha256:{B}","size":1}},
                {{"name":"b.zip","digest":null}},
                {{"name":"c.zip"}},
                {{"name":"my app.exe","digest":"sha256:{A}"}}]}}"#
        );
        assert_eq!(digest_of(&json, "my app.exe"), sha256(A));
        assert_eq!(digest_of(&json, "a.zip"), sha256(B));
        assert_eq!(digest_of(&json, "b.zip"), None);
        assert_eq!(digest_of(&json, "c.zip"), None);
        assert_eq!(digest_of(&json, "d.zip"), None);
        assert_eq!(digest_of("{\"message\":\"Not Found\"}", "a.zip"), None);
    }
}
