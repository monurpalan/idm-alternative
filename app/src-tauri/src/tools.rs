//! Helper programs for video: yt-dlp (sites such as YouTube) and deno (which yt-dlp needs
//! for YouTube's player code), set up on request into `<data>/tools` from their official
//! GitHub releases and checked against the SHA-256 the release publishes. ffmpeg is used
//! from PATH when installed.

use std::path::{Path, PathBuf};

use shardload_engine::{CancellationToken, Options, Request};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Ytdlp,
    Deno,
}

struct Source {
    exe: &'static str,
    /// GitHub's API answer for the latest release, which names its tag.
    latest: &'static str,
    /// What the release offers for this system (Windows or Linux, x64).
    url: &'static str,
    /// The release's checksum file.
    sums: &'static str,
    /// The file the checksum is listed for.
    file: &'static str,
    /// Inside a zip: the member that is the executable.
    zip_member: Option<&'static str>,
}

#[cfg(windows)]
fn source(tool: Tool) -> Source {
    match tool {
        Tool::Ytdlp => Source {
            exe: "yt-dlp.exe",
            latest: "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest",
            url: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe",
            sums: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS",
            file: "yt-dlp.exe",
            zip_member: None,
        },
        Tool::Deno => Source {
            exe: "deno.exe",
            latest: "https://api.github.com/repos/denoland/deno/releases/latest",
            url: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip",
            sums: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip.sha256sum",
            file: "deno-x86_64-pc-windows-msvc.zip",
            zip_member: Some("deno.exe"),
        },
    }
}

#[cfg(not(windows))]
fn source(tool: Tool) -> Source {
    match tool {
        Tool::Ytdlp => Source {
            exe: "yt-dlp",
            latest: "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest",
            url: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux",
            sums: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS",
            file: "yt-dlp_linux",
            zip_member: None,
        },
        Tool::Deno => Source {
            exe: "deno",
            latest: "https://api.github.com/repos/denoland/deno/releases/latest",
            url: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip",
            sums: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip.sha256sum",
            file: "deno-x86_64-unknown-linux-gnu.zip",
            zip_member: Some("deno"),
        },
    }
}

pub fn dir(data: &Path) -> PathBuf {
    data.join("tools")
}

/// The tool's executable, if it is set up.
pub fn installed(data: &Path, tool: Tool) -> Option<PathBuf> {
    let p = dir(data).join(source(tool).exe);
    p.is_file().then_some(p)
}

/// The SHA-256 a checksum file gives for `file`: from `<hash>  <file>` lines (yt-dlp), or
/// the only hash in the file (deno's `Get-FileHash` output).
pub fn checksum_for(text: &str, file: &str) -> Option<String> {
    let is_hash = |w: &str| w.len() == 64 && w.bytes().all(|b| b.is_ascii_hexdigit());
    let hash_in = |line: &str| {
        line.split(|c: char| c.is_whitespace() || c == ':' || c == '*')
            .find(|w| is_hash(w))
            .map(str::to_ascii_lowercase)
    };
    let named = text
        .lines()
        .filter(|l| l.trim_end().ends_with(file))
        .find_map(hash_in);
    named.or_else(|| {
        let all: Vec<String> = text.lines().filter_map(hash_in).collect();
        (all.len() == 1).then(|| all[0].clone())
    })
}

/// The tool's version, by running it.
pub async fn version(exe: &Path) -> Option<String> {
    let out = tokio::process::Command::new(exe)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    out.status
        .success()
        .then(|| text.lines().next().unwrap_or_default().trim().to_owned())
}

/// The version number in a tool's `--version` line (`deno 2.9.7 (stable…)`, `2026.08.19`) or
/// a release tag (`v2.9.7`).
pub fn version_number(text: &str) -> Option<&str> {
    text.split_whitespace()
        .map(|w| w.strip_prefix('v').unwrap_or(w))
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
}

/// The version of the tool's latest release, from GitHub's API.
pub async fn latest(tool: Tool, opts: &Options) -> Result<String, String> {
    let headers = [(
        "Accept".to_owned(),
        "application/vnd.github+json".to_owned(),
    )];
    let (text, _) = shardload_engine::fetch_text(source(tool).latest, &headers, opts, 1 << 20)
        .await
        .map_err(|e| format!("could not ask GitHub for the latest release: {e}"))?;
    let release: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("GitHub answered something unexpected: {e}"))?;
    release["tag_name"]
        .as_str()
        .and_then(version_number)
        .map(str::to_owned)
        .ok_or_else(|| "GitHub's answer names no release".to_owned())
}

/// Updates the tool when it is set up and its latest release is a different version.
/// Returns the new version, or `None` when it was already current or is not set up.
pub async fn update(data: &Path, tool: Tool, opts: &Options) -> Result<Option<String>, String> {
    let Some(exe) = installed(data, tool) else {
        return Ok(None);
    };
    let have = version(&exe).await;
    let latest = latest(tool, opts).await?;
    if have.as_deref().and_then(version_number) == Some(latest.as_str()) {
        return Ok(None);
    }
    let exe = install(data, tool, opts).await?;
    Ok(version(&exe).await)
}

/// Downloads the latest release of `tool` into the tools folder, verified, and returns the
/// executable. Replaces an older copy only once the new one is complete and checked.
pub async fn install(data: &Path, tool: Tool, opts: &Options) -> Result<PathBuf, String> {
    let src = source(tool);
    let tools = dir(data);
    std::fs::create_dir_all(&tools).map_err(|e| e.to_string())?;
    let (sums, _) = shardload_engine::fetch_text(src.sums, &[], opts, 1 << 20)
        .await
        .map_err(|e| format!("could not read the release's checksums: {e}"))?;
    let hash = checksum_for(&sums, src.file)
        .ok_or_else(|| "the release lists no checksum for this system".to_owned())?;

    let download = tools.join(format!("{}.download", src.file));
    std::fs::remove_file(&download).ok();
    let opts = Options {
        checksum: Some(format!("sha256:{hash}").parse().map_err(|e: String| e)?),
        connections: 8,
        ..opts.clone()
    };
    shardload_engine::download(
        &Request {
            url: src.url.to_owned(),
            output: download.clone(),
            headers: Vec::new(),
            mirrors: Vec::new(),
        },
        &opts,
        None,
        &CancellationToken::new(),
    )
    .await
    .map_err(|e| format!("could not download {}: {e}", src.exe))?;

    let exe = tools.join(src.exe);
    let fresh = tools.join(format!("{}.new", src.exe));
    match src.zip_member {
        None => std::fs::rename(&download, &fresh).map_err(|e| e.to_string())?,
        Some(member) => {
            let unzip = || -> Result<(), String> {
                let file = std::fs::File::open(&download).map_err(|e| e.to_string())?;
                let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
                let mut entry = zip.by_name(member).map_err(|e| e.to_string())?;
                let mut out = std::fs::File::create(&fresh).map_err(|e| e.to_string())?;
                std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
                Ok(())
            };
            let unzipped = unzip();
            std::fs::remove_file(&download).ok();
            unzipped?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    if version(&fresh).await.is_none() {
        std::fs::remove_file(&fresh).ok();
        return Err(format!("the downloaded {} does not run", src.exe));
    }
    std::fs::rename(&fresh, &exe).map_err(|e| e.to_string())?;
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against GitHub: an older yt-dlp in `SHARDLOAD_TOOLS_TEST` (a folder with `tools/yt-dlp.exe`)
    /// is replaced by the latest release, and a second run finds nothing to do.
    #[tokio::test]
    #[ignore = "downloads from GitHub"]
    async fn updates_an_old_ytdlp() {
        let data =
            PathBuf::from(std::env::var("SHARDLOAD_TOOLS_TEST").expect("SHARDLOAD_TOOLS_TEST"));
        let opts = Options::default();
        let latest = latest(Tool::Ytdlp, &opts).await.expect("latest");
        let updated = update(&data, Tool::Ytdlp, &opts).await.expect("update");
        assert_eq!(updated.as_deref(), Some(latest.as_str()));
        assert_eq!(
            update(&data, Tool::Ytdlp, &opts).await.expect("again"),
            None
        );
        // Not set up: nothing to update.
        assert_eq!(update(&data, Tool::Deno, &opts).await.expect("deno"), None);
    }

    #[test]
    fn version_numbers() {
        assert_eq!(version_number("2026.08.19"), Some("2026.08.19"));
        assert_eq!(
            version_number("deno 2.9.7 (stable, release, x86_64-pc-windows-msvc)"),
            Some("2.9.7")
        );
        assert_eq!(version_number("v2.9.7"), Some("2.9.7"));
        assert_eq!(version_number("deno"), None);
    }

    #[test]
    fn checksum_files() {
        let ytdlp = "aaaa  yt-dlp\n66674953fe251b89f4d08c5f0e35e0728679bd67ab3d7d05c0562af101dd3e7a  yt-dlp.exe\n1111111111111111111111111111111111111111111111111111111111111111  yt-dlp_x86.exe\n";
        assert_eq!(
            checksum_for(ytdlp, "yt-dlp.exe").as_deref(),
            Some("66674953fe251b89f4d08c5f0e35e0728679bd67ab3d7d05c0562af101dd3e7a")
        );
        let deno = "\r\nAlgorithm : SHA256\r\nHash      : A0C3101B4158D1DFB7D6A78A7BF0F3DE80C96BB423C152BEEC8BEB22786F2238\r\nPath      : C:\\a\\deno\\target\\release\\deno-x86_64-pc-windows-msvc.zip\r\n";
        assert_eq!(
            checksum_for(deno, "deno-x86_64-pc-windows-msvc.zip").as_deref(),
            Some("a0c3101b4158d1dfb7d6a78a7bf0f3de80c96bb423c152beec8beb22786f2238")
        );
        assert_eq!(checksum_for("nothing here", "x"), None);
        // Two unnamed hashes: ambiguous.
        let two = format!("{}\n{}\n", "a".repeat(64), "b".repeat(64));
        assert_eq!(checksum_for(&two, "x"), None);
    }
}
