//! Helper programs for video: yt-dlp (sites such as YouTube), deno (which yt-dlp needs for
//! YouTube's player code) and ffmpeg (which joins separate video and audio into one file),
//! set up into `<data>/tools` from official GitHub releases and checked against the SHA-256
//! the release publishes. yt-dlp and deno are set up on request; ffmpeg by itself the first
//! time a download needs it, unless one is on PATH.

use std::path::{Path, PathBuf};

use shardload_engine::{CancellationToken, Options, Progress, Request};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Ytdlp,
    Deno,
    Ffmpeg,
}

struct Source {
    /// The program, relative to the tools folder.
    exe: &'static str,
    /// GitHub's API answer for the latest release, which names its tag and files.
    latest: &'static str,
    files: Files,
}

/// Which file of the release is downloaded, and how the program comes out of it.
enum Files {
    /// Fixed addresses in the latest release: the file (the program itself, or a zip with
    /// the program as `zip_member`) and the checksum file that lists it.
    Fixed {
        url: &'static str,
        sums: &'static str,
        file: &'static str,
        zip_member: Option<&'static str>,
    },
    /// BtbN's ffmpeg builds (the ones ffmpeg.org links to): the newest release branch,
    /// `ffmpeg-n<X.Y>-latest-<build>-<X.Y><ext>`, listed in `checksums.sha256`. The programs
    /// in its `bin` folder go into a folder of their own.
    Ffmpeg {
        build: &'static str,
        ext: &'static str,
    },
}

const FFMPEG_LATEST: &str = "https://api.github.com/repos/BtbN/FFmpeg-Builds/releases/latest";

#[cfg(windows)]
fn source(tool: Tool) -> Source {
    match tool {
        Tool::Ytdlp => Source {
            exe: "yt-dlp.exe",
            latest: "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest",
            files: Files::Fixed {
                url: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe",
                sums: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS",
                file: "yt-dlp.exe",
                zip_member: None,
            },
        },
        Tool::Deno => Source {
            exe: "deno.exe",
            latest: "https://api.github.com/repos/denoland/deno/releases/latest",
            files: Files::Fixed {
                url: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip",
                sums: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip.sha256sum",
                file: "deno-x86_64-pc-windows-msvc.zip",
                zip_member: Some("deno.exe"),
            },
        },
        // The shared build: its DLLs are found next to the programs, and it is less than half
        // the size of the static one, where each program carries all of them.
        Tool::Ffmpeg => Source {
            exe: "ffmpeg/ffmpeg.exe",
            latest: FFMPEG_LATEST,
            files: Files::Ffmpeg {
                build: "win64-lgpl-shared",
                ext: ".zip",
            },
        },
    }
}

#[cfg(not(windows))]
fn source(tool: Tool) -> Source {
    match tool {
        Tool::Ytdlp => Source {
            exe: "yt-dlp",
            latest: "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest",
            files: Files::Fixed {
                url: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux",
                sums: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS",
                file: "yt-dlp_linux",
                zip_member: None,
            },
        },
        Tool::Deno => Source {
            exe: "deno",
            latest: "https://api.github.com/repos/denoland/deno/releases/latest",
            files: Files::Fixed {
                url: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip",
                sums: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip.sha256sum",
                file: "deno-x86_64-unknown-linux-gnu.zip",
                zip_member: Some("deno"),
            },
        },
        // The static build: the shared one's programs do not look for their libraries next
        // to them (no `$ORIGIN` run path).
        Tool::Ffmpeg => Source {
            exe: "ffmpeg/ffmpeg",
            latest: FFMPEG_LATEST,
            files: Files::Ffmpeg {
                build: "linux64-lgpl",
                ext: ".tar.xz",
            },
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

/// The SHA-256 a checksum file gives for `file`: from `<hash>  <file>` lines (yt-dlp,
/// ffmpeg), or the only hash in the file (deno's `Get-FileHash` output).
pub fn checksum_for(text: &str, file: &str) -> Option<String> {
    let is_hash = |w: &str| w.len() == 64 && w.bytes().all(|b| b.is_ascii_hexdigit());
    let hash_in = |line: &str| {
        line.split(|c: char| c.is_whitespace() || c == ':' || c == '*')
            .find(|w| is_hash(w))
            .map(str::to_ascii_lowercase)
    };
    let named = text
        .lines()
        .filter(|l| {
            let l = l.trim_end();
            // The whole name: `x.zip` must not match a line for `y-x.zip`.
            l.strip_suffix(file).is_some_and(|before| {
                before.is_empty()
                    || before.ends_with(|c: char| c.is_whitespace() || "*:\\/".contains(c))
            })
        })
        .find_map(hash_in);
    named.or_else(|| {
        let all: Vec<String> = text.lines().filter_map(hash_in).collect();
        (all.len() == 1).then(|| all[0].clone())
    })
}

/// The tool's version, by running it: the first line of `--version` (`-version` for ffmpeg,
/// shortened to the version itself).
pub async fn version(exe: &Path) -> Option<String> {
    let is_ffmpeg = exe
        .file_stem()
        .is_some_and(|s| s.eq_ignore_ascii_case("ffmpeg"));
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg(if is_ffmpeg { "-version" } else { "--version" })
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().await.ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or_default().trim();
    if !out.status.success() {
        return None;
    }
    Some(match line.strip_prefix("ffmpeg version ") {
        Some(rest) => rest
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned(),
        None => line.to_owned(),
    })
}

/// The version number in a tool's `--version` line (`deno 2.9.7 (stable…)`, `2026.08.19`),
/// ffmpeg's version (`n9.0.2-28-g669f5ab083-20261018`, `7.1-essentials_build-www.gyan.dev`)
/// or a release tag (`v2.9.7`).
pub fn version_number(text: &str) -> Option<&str> {
    text.split_whitespace()
        .map(|w| w.strip_prefix(['v', 'n']).unwrap_or(w))
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
        .map(|w| w.split('-').next().unwrap_or(w))
}

/// The release branch of an ffmpeg version (`9.0` for `n9.0.2-28-g…`): ffmpeg is updated
/// when a new branch comes out, not for each day's build of the same one.
fn branch(version: &str) -> Option<String> {
    let mut parts = version_number(version)?.split('.');
    let major = parts.next().filter(|p| !p.is_empty())?;
    let minor = parts.next().unwrap_or("0");
    Some(format!("{major}.{minor}"))
}

/// What a release offers for this system: the file to download and the checksum file.
struct Release {
    url: String,
    sums: String,
    file: String,
    /// The version the release's file is, when known before running it (ffmpeg's branch).
    version: Option<String>,
}

/// The newest release branch's build of ffmpeg in GitHub's answer for BtbN's latest release.
fn ffmpeg_release(release: &serde_json::Value, build: &str, ext: &str) -> Option<Release> {
    let assets = release["assets"].as_array()?;
    let url_of = |name: &str| {
        assets
            .iter()
            .find(|a| a["name"].as_str() == Some(name))
            .and_then(|a| a["browser_download_url"].as_str())
            .map(str::to_owned)
    };
    let (file, version) = assets
        .iter()
        .filter_map(|a| a["name"].as_str())
        .filter_map(|name| {
            let rest = name.strip_prefix("ffmpeg-n")?;
            let (version, rest) = rest.split_once("-latest-")?;
            let tail = rest.strip_prefix(build)?.strip_prefix('-')?;
            (tail.strip_suffix(ext)? == version).then_some(())?;
            let key: Vec<u32> = version
                .split('.')
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            Some((key, name, version))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, name, version)| (name.to_owned(), version.to_owned()))?;
    Some(Release {
        url: url_of(&file)?,
        sums: url_of("checksums.sha256")?,
        file,
        version: Some(version),
    })
}

async fn latest_release(tool: Tool, opts: &Options) -> Result<serde_json::Value, String> {
    let headers = [(
        "Accept".to_owned(),
        "application/vnd.github+json".to_owned(),
    )];
    let (text, _) = shardload_engine::fetch_text(source(tool).latest, &headers, opts, 4 << 20)
        .await
        .map_err(|e| format!("could not ask GitHub for the latest release: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("GitHub answered something unexpected: {e}"))
}

async fn release(tool: Tool, opts: &Options) -> Result<Release, String> {
    match source(tool).files {
        Files::Fixed {
            url, sums, file, ..
        } => Ok(Release {
            url: url.to_owned(),
            sums: sums.to_owned(),
            file: file.to_owned(),
            version: None,
        }),
        Files::Ffmpeg { build, ext } => {
            let json = latest_release(tool, opts).await?;
            ffmpeg_release(&json, build, ext)
                .ok_or_else(|| "the release has no ffmpeg build for this system".to_owned())
        }
    }
}

/// The version of the tool's latest release, from GitHub's API (ffmpeg: its newest branch).
pub async fn latest(tool: Tool, opts: &Options) -> Result<String, String> {
    if let Files::Ffmpeg { .. } = source(tool).files {
        return release(tool, opts)
            .await?
            .version
            .ok_or_else(|| "GitHub's answer names no release".to_owned());
    }
    latest_release(tool, opts).await?["tag_name"]
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
    let current = match tool {
        Tool::Ffmpeg => have.as_deref().and_then(branch) == Some(latest),
        _ => have.as_deref().and_then(version_number) == Some(latest.as_str()),
    };
    if current {
        return Ok(None);
    }
    let exe = install(data, tool, opts, None, &CancellationToken::new()).await?;
    Ok(version(&exe).await)
}

/// Downloads the latest release of `tool` into the tools folder, verified, and returns the
/// executable. Replaces an older copy only once the new one is complete and checked.
/// `progress` follows the download of the release's file.
pub async fn install(
    data: &Path,
    tool: Tool,
    opts: &Options,
    progress: Option<&watch::Sender<Progress>>,
    cancel: &CancellationToken,
) -> Result<PathBuf, String> {
    let src = source(tool);
    let tools = dir(data);
    std::fs::create_dir_all(&tools).map_err(|e| e.to_string())?;
    let release = release(tool, opts).await?;
    let (sums, _) = shardload_engine::fetch_text(&release.sums, &[], opts, 1 << 20)
        .await
        .map_err(|e| format!("could not read the release's checksums: {e}"))?;
    let hash = checksum_for(&sums, &release.file)
        .ok_or_else(|| "the release lists no checksum for this system".to_owned())?;

    let download = tools.join(format!("{}.download", release.file));
    std::fs::remove_file(&download).ok();
    let opts = Options {
        checksum: Some(format!("sha256:{hash}").parse().map_err(|e: String| e)?),
        connections: 8,
        ..opts.clone()
    };
    let name = Path::new(src.exe)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let got = shardload_engine::download(
        &Request {
            url: release.url.clone(),
            output: download.clone(),
            headers: Vec::new(),
            mirrors: Vec::new(),
        },
        &opts,
        progress,
        cancel,
    )
    .await;
    if let Err(e) = got {
        std::fs::remove_file(&download).ok();
        return Err(if cancel.is_cancelled() {
            "cancelled".to_owned()
        } else {
            format!("could not download {name}: {e}")
        });
    }

    let exe = tools.join(src.exe);
    match src.files {
        Files::Fixed { zip_member, .. } => {
            let fresh = tools.join(format!("{}.new", src.exe));
            match zip_member {
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
            make_runnable(&fresh)?;
            if version(&fresh).await.is_none() {
                std::fs::remove_file(&fresh).ok();
                return Err(format!("the downloaded {name} does not run"));
            }
            std::fs::rename(&fresh, &exe).map_err(|e| e.to_string())?;
        }
        Files::Ffmpeg { .. } => {
            // A folder of programs (and, on Windows, their DLLs): unpacked next to the old one,
            // tried, then swapped in.
            let folder = exe.parent().expect("in a folder").to_owned();
            let fresh = folder.with_extension("new");
            let old = folder.with_extension("old");
            std::fs::remove_dir_all(&fresh).ok();
            std::fs::create_dir_all(&fresh).map_err(|e| e.to_string())?;
            let (archive, into) = (download.clone(), fresh.clone());
            let unpacked = tokio::task::spawn_blocking(move || unpack_bin(&archive, &into))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            std::fs::remove_file(&download).ok();
            let fresh_exe = fresh.join(exe.file_name().expect("a file"));
            let runs = match unpacked {
                Ok(_) => version(&fresh_exe).await.is_some(),
                Err(e) => {
                    std::fs::remove_dir_all(&fresh).ok();
                    return Err(format!("could not unpack {name}: {e}"));
                }
            };
            if !runs {
                std::fs::remove_dir_all(&fresh).ok();
                return Err(format!("the downloaded {name} does not run"));
            }
            std::fs::remove_dir_all(&old).ok();
            if folder.exists() {
                std::fs::rename(&folder, &old)
                    .map_err(|e| format!("could not replace the old {name} (in use?): {e}"))?;
            }
            std::fs::rename(&fresh, &folder).map_err(|e| e.to_string())?;
            std::fs::remove_dir_all(&old).ok();
        }
    }
    Ok(exe)
}

fn make_runnable(_file: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(_file, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Whether an archive member is one to keep: a file directly in a `bin` folder, but not
/// ffplay (a player, not needed to join files).
fn wanted(path: &Path) -> Option<&std::ffi::OsStr> {
    let name = path.file_name()?;
    let parent = path.parent()?.file_name()?;
    let is_player = name
        .to_string_lossy()
        .to_ascii_lowercase()
        .starts_with("ffplay");
    (parent == "bin" && !is_player).then_some(name)
}

/// Copies the programs in the `bin` folder of a `.zip` or `.tar.xz` archive into `to`, and
/// returns how many there were.
fn unpack_bin(archive: &Path, to: &Path) -> Result<usize, String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut count = 0;
    let mut keep = |path: &Path, from: &mut dyn std::io::Read| -> Result<(), String> {
        let Some(name) = wanted(path) else {
            return Ok(());
        };
        let target = to.join(name);
        let mut out = std::fs::File::create(&target).map_err(|e| e.to_string())?;
        std::io::copy(from, &mut out).map_err(|e| e.to_string())?;
        make_runnable(&target)?;
        count += 1;
        Ok(())
    };
    if archive.to_string_lossy().ends_with(".zip.download")
        || archive.extension().is_some_and(|e| e == "zip")
    {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
            if !entry.is_file() {
                continue;
            }
            let Some(path) = entry.enclosed_name() else {
                continue;
            };
            keep(&path, &mut entry)?;
        }
    } else {
        // xz decoded in a thread, read as a tar through a pipe: the archive is never on disk
        // unpacked whole.
        let (reader, mut writer) = std::io::pipe().map_err(|e| e.to_string())?;
        let decoder = std::thread::spawn(move || {
            let mut input = std::io::BufReader::new(file);
            lzma_rs::xz_decompress(&mut input, &mut writer).map_err(|e| format!("{e:?}"))
        });
        let read = (|| -> Result<(), String> {
            let mut tar = tar::Archive::new(reader);
            for entry in tar.entries().map_err(|e| e.to_string())? {
                let mut entry = entry.map_err(|e| e.to_string())?;
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let path = entry.path().map_err(|e| e.to_string())?.into_owned();
                keep(&path, &mut entry)?;
            }
            Ok(())
        })();
        let decoded = decoder
            .join()
            .map_err(|_| "the xz decoder stopped".to_owned())?;
        read?;
        decoded?;
    }
    if count == 0 {
        return Err("the archive has no programs in a bin folder".into());
    }
    Ok(count)
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

    /// Against GitHub: ffmpeg is set up into an empty data folder with progress, runs, can
    /// join files (it lists the mp4 muxer), and is then current.
    #[tokio::test]
    #[ignore = "downloads ffmpeg from GitHub (about 80 MB on Windows, 140 MB on Linux)"]
    async fn sets_up_ffmpeg() {
        let data = std::env::temp_dir().join(format!("shardload-ffmpeg-{}", std::process::id()));
        std::fs::remove_dir_all(&data).ok();
        let opts = Options::default();
        assert_eq!(
            update(&data, Tool::Ffmpeg, &opts).await.expect("none"),
            None
        );
        let (tx, rx) = watch::channel(Progress::default());
        let exe = install(
            &data,
            Tool::Ffmpeg,
            &opts,
            Some(&tx),
            &CancellationToken::new(),
        )
        .await
        .expect("install");
        assert!(rx.borrow().received > 10 << 20, "progress was reported");
        assert_eq!(installed(&data, Tool::Ffmpeg), Some(exe.clone()));
        let have = version(&exe).await.expect("runs");
        let latest = latest(Tool::Ffmpeg, &opts).await.expect("latest");
        assert_eq!(branch(&have), Some(latest), "{have}");
        let muxers = std::process::Command::new(&exe)
            .args(["-hide_banner", "-muxers"])
            .output()
            .expect("muxers");
        assert!(String::from_utf8_lossy(&muxers.stdout).contains("mp4"));
        let names: Vec<String> = std::fs::read_dir(exe.parent().expect("folder"))
            .expect("fixture")
            .map(|e| {
                e.expect("fixture")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(names.iter().any(|n| n.starts_with("ffprobe")), "{names:?}");
        assert!(!names.iter().any(|n| n.starts_with("ffplay")), "{names:?}");
        assert_eq!(
            update(&data, Tool::Ffmpeg, &opts).await.expect("again"),
            None
        );
        std::fs::remove_dir_all(&data).ok();
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
        assert_eq!(
            version_number("n9.0.2-28-g669f5ab083-20261018"),
            Some("9.0.2")
        );
        assert_eq!(
            branch("n9.0.2-28-g669f5ab083-20261018").as_deref(),
            Some("9.0")
        );
        assert_eq!(
            branch("7.1-essentials_build-www.gyan.dev").as_deref(),
            Some("7.1")
        );
        assert_eq!(branch("9").as_deref(), Some("9.0"));
        // A build of ffmpeg's development branch has no release number.
        assert_eq!(branch("N-121234-gabcdef-20261018"), None);
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
        // ffmpeg's list: one name must not match another that ends with it.
        let ffmpeg = format!(
            "{}  ffmpeg-n9.0-latest-win64-lgpl-shared-9.0.zip\n{}  ffmpeg-n9.0-latest-win64-gpl-shared-9.0.zip\n{}  ffmpeg-n9.0-latest-linux64-lgpl-9.0.tar.xz\n",
            "1".repeat(64),
            "2".repeat(64),
            "3".repeat(64)
        );
        assert_eq!(
            checksum_for(&ffmpeg, "ffmpeg-n9.0-latest-win64-gpl-shared-9.0.zip"),
            Some("2".repeat(64))
        );
        assert_eq!(checksum_for(&ffmpeg, "gpl-shared-9.0.zip"), None);
    }

    #[test]
    fn picks_the_newest_ffmpeg_branch() {
        let asset = |name: &str| {
            serde_json::json!({
                "name": name,
                "browser_download_url": format!("https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/{name}")
            })
        };
        let names = [
            "checksums.sha256",
            "ffmpeg-master-latest-win64-lgpl-shared.zip",
            "ffmpeg-n8.1-latest-win64-lgpl-shared-8.1.zip",
            "ffmpeg-n9.0-latest-win64-lgpl-shared-9.0.zip",
            "ffmpeg-n9.0-latest-win64-gpl-shared-9.0.zip",
            "ffmpeg-n10.0-latest-win64-lgpl-10.0.zip",
            "ffmpeg-n8.1-latest-linux64-lgpl-8.1.tar.xz",
            "ffmpeg-n9.0-latest-linux64-lgpl-9.0.tar.xz",
            "ffmpeg-n9.0-latest-linux64-lgpl-shared-9.0.tar.xz",
        ];
        let json = serde_json::json!({
            "tag_name": "latest",
            "assets": names.iter().map(|n| asset(n)).collect::<Vec<_>>()
        });
        let win = ffmpeg_release(&json, "win64-lgpl-shared", ".zip").expect("windows");
        assert_eq!(win.file, "ffmpeg-n9.0-latest-win64-lgpl-shared-9.0.zip");
        assert_eq!(win.version.as_deref(), Some("9.0"));
        assert!(
            win.url
                .ends_with("/latest/ffmpeg-n9.0-latest-win64-lgpl-shared-9.0.zip")
        );
        assert!(win.sums.ends_with("/latest/checksums.sha256"));
        let linux = ffmpeg_release(&json, "linux64-lgpl", ".tar.xz").expect("linux");
        assert_eq!(linux.file, "ffmpeg-n9.0-latest-linux64-lgpl-9.0.tar.xz");
        assert!(ffmpeg_release(&json, "win64-gpl", ".zip").is_none());
        // Without a checksum list, nothing can be verified: no release.
        let unlisted = serde_json::json!({ "assets": [asset(names[3])] });
        assert!(ffmpeg_release(&unlisted, "win64-lgpl-shared", ".zip").is_none());
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shardload-unpack-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("out")).expect("fixture");
        dir
    }

    const MEMBERS: [(&str, &[u8]); 5] = [
        ("ffmpeg-n9.0/bin/ffmpeg.exe", b"ffmpeg"),
        ("ffmpeg-n9.0/bin/ffprobe.exe", b"ffprobe"),
        ("ffmpeg-n9.0/bin/avcodec-63.dll", b"dll"),
        ("ffmpeg-n9.0/bin/ffplay.exe", b"player"),
        ("ffmpeg-n9.0/doc/ffmpeg.html", b"doc"),
    ];

    fn check(out: &Path) {
        let mut names: Vec<String> = std::fs::read_dir(out)
            .expect("fixture")
            .map(|e| {
                e.expect("fixture")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        assert_eq!(names, ["avcodec-63.dll", "ffmpeg.exe", "ffprobe.exe"]);
        assert_eq!(
            std::fs::read(out.join("ffmpeg.exe")).expect("fixture"),
            b"ffmpeg"
        );
    }

    #[test]
    fn unpacks_bin_from_a_zip() {
        use std::io::Write;
        let dir = scratch("zip");
        let archive = dir.join("ffmpeg-n9.0.zip.download");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).expect("fixture"));
        let opts = zip::write::SimpleFileOptions::default();
        zip.add_directory("ffmpeg-n9.0/bin/", opts)
            .expect("fixture");
        for (name, data) in MEMBERS {
            zip.start_file(name, opts).expect("fixture");
            zip.write_all(data).expect("fixture");
        }
        zip.finish().expect("fixture");
        assert_eq!(unpack_bin(&archive, &dir.join("out")), Ok(3));
        check(&dir.join("out"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpacks_bin_from_a_tar_xz() {
        let dir = scratch("tar");
        let mut tar = tar::Builder::new(Vec::new());
        for (name, data) in MEMBERS {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, name, data).expect("fixture");
        }
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_cksum();
        tar.append_link(&mut header, "ffmpeg-n9.0/bin/link", "ffmpeg.exe")
            .expect("fixture");
        let plain = tar.into_inner().expect("fixture");
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut plain.as_slice(), &mut xz).expect("fixture");
        let archive = dir.join("ffmpeg-n9.0.tar.xz.download");
        std::fs::write(&archive, xz).expect("fixture");
        assert_eq!(unpack_bin(&archive, &dir.join("out")), Ok(3));
        check(&dir.join("out"));
        // Not an archive.
        std::fs::write(&archive, b"junk").expect("fixture");
        assert!(unpack_bin(&archive, &dir.join("out")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
