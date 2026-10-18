use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use idma_engine::{
    Browser, CancellationToken, Checksum, Conflict, Error, Limiter, Options, Phase, Progress,
    RemoteZip, Request,
};

/// Command-line front end for the idm-alternative download engine.
#[derive(Parser)]
#[command(name = "dm", version = idma_engine::VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download a URL. Interrupted downloads resume when run again with the same arguments.
    Get {
        url: String,
        /// Output file name, or path; by default the name the server suggests
        /// (`Content-Disposition`) or the last part of the URL. An existing file with this
        /// exact name is replaced.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Folder to save into (default: the current folder).
        #[arg(short = 'd', long = "dir")]
        dir: Option<PathBuf>,
        /// Without `-o`: replace an existing file of the same name instead of saving as
        /// `name (1).ext`.
        #[arg(long)]
        overwrite: bool,
        /// Maximum simultaneous connections.
        #[arg(short, long, default_value_t = 16)]
        connections: usize,
        /// Speed limit in bytes per second; `K`, `M`, `G` suffixes (binary units), e.g. `2M`.
        #[arg(short, long, value_parser = parse_rate)]
        limit: Option<u64>,
        #[command(flatten)]
        net: Net,
        /// Verify the finished file: `sha256:<hex>` (also md5, sha1, sha512), or a bare hex
        /// digest.
        #[arg(long)]
        checksum: Option<Checksum>,
        /// Without `--checksum`, do not look for a published one (GitHub's digest of a
        /// release asset, or `<file>.sha256`, `SHA256SUMS` and the like next to the file).
        #[arg(long)]
        no_checksum_search: bool,
        /// Another address of the same file (repeatable). Used alongside the URL once it
        /// serves the same bytes; dropped if it fails or crawls.
        #[arg(short = 'm', long = "mirror")]
        mirrors: Vec<String>,
        /// No progress line.
        #[arg(short, long)]
        quiet: bool,
    },
    /// List or pick files out of a ZIP archive on a server without downloading all of it
    /// (the server must support range requests).
    Zip {
        #[command(subcommand)]
        command: ZipCommand,
    },
}

#[derive(Subcommand)]
enum ZipCommand {
    /// List the files in the archive: size, packed size, compression and name.
    List {
        url: String,
        #[command(flatten)]
        net: Net,
    },
    /// Download only the named files. Folders keep their structure under `--out-dir`. A
    /// server without range support gets the whole archive downloaded instead.
    Get {
        url: String,
        /// Entry names, folders (everything in them) or globs: `*` and `?` within a folder,
        /// `**` across folders, e.g. `docs/**/*.pdf`.
        #[arg(required = true)]
        entries: Vec<String>,
        /// Folder to extract into (default: the current folder).
        #[arg(short = 'd', long = "out-dir")]
        out_dir: Option<PathBuf>,
        /// Replace files that are already there; by default a file of the same size is
        /// taken as done and a different one is left alone.
        #[arg(long)]
        overwrite: bool,
        /// Speed limit in bytes per second; `K`, `M`, `G` suffixes (binary units), e.g. `2M`.
        #[arg(short, long, value_parser = parse_rate)]
        limit: Option<u64>,
        #[command(flatten)]
        net: Net,
        /// No progress line.
        #[arg(short, long)]
        quiet: bool,
    },
}

/// How to reach the server.
#[derive(Args)]
struct Net {
    /// Extra request header, `Name: value` (repeatable).
    #[arg(short = 'H', long = "header")]
    headers: Vec<String>,
    /// Browser fingerprint (TLS, HTTP/2, headers): chrome, edge, firefox, safari or none.
    // Fully qualified so clap does not treat it as an optional argument: the parser
    // itself returns `None` for "none".
    #[arg(long, default_value = "chrome", value_parser = parse_browser)]
    browser: std::option::Option<Browser>,
    /// User-Agent; by default the browser's own.
    #[arg(long)]
    user_agent: Option<String>,
    /// Proxy for every request: `http://`, `https://` or `socks5://host:port`.
    #[arg(long)]
    proxy: Option<String>,
    /// Accept invalid or self-signed TLS certificates.
    #[arg(short = 'k', long)]
    insecure: bool,
    /// Never use HTTP/2.
    #[arg(long)]
    http1: bool,
}

impl Net {
    /// The request headers, and the engine options without what only a download sets.
    fn split(self) -> Result<(Vec<(String, String)>, Options), ExitCode> {
        let headers = self
            .headers
            .iter()
            .map(|h| {
                h.split_once(':')
                    .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                    .ok_or_else(|| format!("header must be `Name: value`, got `{h}`"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                eprintln!("dm: {e}");
                ExitCode::from(2)
            })?;
        let opts = Options {
            browser: self.browser,
            user_agent: self.user_agent,
            proxy: self.proxy,
            insecure: self.insecure,
            http1_only: self.http1,
            ..Options::default()
        };
        Ok((headers, opts))
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("DM_LOG").unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().command {
        Command::Get {
            url,
            output,
            dir,
            overwrite,
            connections,
            limit,
            net,
            checksum,
            no_checksum_search,
            mirrors,
            quiet,
        } => {
            let (headers, opts) = match net.split() {
                Ok(h) => h,
                Err(code) => return code,
            };
            let opts = Options {
                connections,
                limiters: limit.map(|r| Limiter::new(Some(r))).into_iter().collect(),
                checksum,
                find_checksum: !no_checksum_search,
                ..opts
            };
            let target = Target {
                output,
                dir,
                overwrite,
            };
            get(url, mirrors, headers, opts, target, quiet).await
        }
        Command::Zip {
            command: ZipCommand::List { url, net },
        } => match net.split() {
            Ok((headers, opts)) => zip_list(&url, &headers, &opts).await,
            Err(code) => code,
        },
        Command::Zip {
            command:
                ZipCommand::Get {
                    url,
                    entries,
                    out_dir,
                    overwrite,
                    limit,
                    net,
                    quiet,
                },
        } => {
            let (headers, opts) = match net.split() {
                Ok(h) => h,
                Err(code) => return code,
            };
            let opts = Options {
                limiters: limit.map(|r| Limiter::new(Some(r))).into_iter().collect(),
                ..opts
            };
            let out_dir = out_dir.unwrap_or_default();
            zip_get(url, &entries, out_dir, overwrite, headers, opts, quiet).await
        }
    }
}

/// Where `dm get` saves.
struct Target {
    output: Option<PathBuf>,
    dir: Option<PathBuf>,
    overwrite: bool,
}

async fn zip_list(url: &str, headers: &[(String, String)], opts: &Options) -> ExitCode {
    let zip = match RemoteZip::open(url, headers, opts).await {
        Ok(z) => z,
        Err(e) => {
            eprintln!("dm: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{:>10}  {:>10}  {:<8}  name", "size", "packed", "method");
    let mut total = 0;
    let mut files = 0;
    for e in zip.entries() {
        let note = match e.unsupported() {
            Some(_) if e.encrypted => "  (encrypted)",
            Some(_) => "  (cannot extract)",
            None => "",
        };
        let method = if e.is_dir {
            String::new()
        } else {
            files += 1;
            total += e.size;
            idma_engine::method_name(e.method)
        };
        println!(
            "{:>10}  {:>10}  {method:<8}  {}{note}",
            if e.is_dir {
                String::new()
            } else {
                human(e.size as f64)
            },
            if e.is_dir {
                String::new()
            } else {
                human(e.compressed_size as f64)
            },
            e.name
        );
    }
    eprintln!(
        "{files} files, {} unpacked; archive {}{}",
        human(total as f64),
        human(zip.size() as f64),
        if zip.is_zip64() { " (ZIP64)" } else { "" }
    );
    ExitCode::SUCCESS
}

async fn zip_get(
    url: String,
    patterns: &[String],
    out_dir: PathBuf,
    overwrite: bool,
    headers: Vec<(String, String)>,
    opts: Options,
    quiet: bool,
) -> ExitCode {
    let zip = match RemoteZip::open(&url, &headers, &opts).await {
        Ok(z) => z,
        Err(e @ Error::NoRanges) => {
            eprintln!("dm: {e}");
            eprintln!(
                "dm: downloading the whole archive to {}",
                display_dir(&out_dir)
            );
            let target = Target {
                output: None,
                dir: Some(out_dir),
                overwrite: false,
            };
            return get(url, Vec::new(), headers, opts, target, quiet).await;
        }
        Err(e) => {
            eprintln!("dm: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (chosen, unmatched) = idma_engine::select_zip_entries(zip.entries(), patterns);
    if !unmatched.is_empty() {
        eprintln!(
            "dm: not in the archive: {} (see `dm zip list`)",
            unmatched.join(", ")
        );
        return ExitCode::from(2);
    }
    let cancel = CancellationToken::new();
    let (tx, rx) = tokio::sync::watch::channel(Progress::default());
    let ctrl_c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            ctrl_c.cancel();
        }
    });
    let printer = (!quiet).then(|| tokio::spawn(print_progress(rx)));
    let result = zip
        .extract(&chosen, &out_dir, overwrite, Some(&tx), &cancel)
        .await;
    if let Some(p) = printer {
        p.abort();
        eprintln!();
    }
    let done = match result {
        Ok(d) => d,
        Err(Error::Cancelled) => {
            eprintln!("dm: cancelled; run the same command again to continue");
            return ExitCode::from(130);
        }
        Err(e) => {
            eprintln!("dm: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = 0;
    for d in done.iter().filter(|d| !d.name.ends_with('/')) {
        match (&d.error, d.skipped) {
            (Some(why), _) => {
                failed += 1;
                eprintln!("failed   {}: {why}", d.name);
            }
            (None, true) => {
                if !quiet {
                    eprintln!("there    {}", d.name);
                }
            }
            (None, false) => {
                if !quiet {
                    eprintln!("saved    {}", d.name);
                }
            }
        }
    }
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn display_dir(dir: &Path) -> String {
    if dir.as_os_str().is_empty() {
        "the current folder".to_owned()
    } else {
        dir.display().to_string()
    }
}

async fn get(
    url: String,
    mirrors: Vec<String>,
    headers: Vec<(String, String)>,
    opts: Options,
    target: Target,
    quiet: bool,
) -> ExitCode {
    let Target {
        output,
        dir,
        overwrite,
    } = target;
    // A metalink: the file it describes, from its addresses, checked against its hash.
    let (url, mirrors, output, opts) = if idma_engine::metalink::is_metalink(&url) {
        match idma_engine::metalink::fetch_metalink(&url, &headers, &opts).await {
            Ok(m) => {
                let mut urls = m.urls.into_iter().take(idma_engine::metalink::MAX_URLS);
                let first = urls.next().unwrap_or_default();
                let (checksum, checksum_from) = match (&opts.checksum, &m.checksum) {
                    (None, Some(c)) => (c.parse().ok(), Some("metalink".to_owned())),
                    _ => (opts.checksum.clone(), None),
                };
                let output = output.or_else(|| Some(m.name.into()));
                let mirrors = urls.chain(mirrors).collect();
                let opts = Options {
                    checksum,
                    checksum_from,
                    ..opts
                };
                (first, mirrors, output, opts)
            }
            Err(e) => {
                eprintln!("dm: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        (url, mirrors, output, opts)
    };
    let dir = dir.unwrap_or_default();
    let output = match output {
        Some(o) => dir.join(o),
        None => match idma_engine::probe(&url, &headers, &opts).await {
            Ok(p) => {
                let conflict = if overwrite {
                    Conflict::Overwrite
                } else {
                    Conflict::Rename
                };
                let path = idma_engine::target(&dir, &p.name, &url, conflict);
                if !quiet {
                    eprintln!("saving as {}", path.display());
                }
                path
            }
            Err(e) => {
                eprintln!("dm: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let req = Request {
        url,
        output,
        headers,
        mirrors,
    };
    let cancel = CancellationToken::new();
    let (tx, rx) = tokio::sync::watch::channel(Progress::default());

    let ctrl_c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            ctrl_c.cancel();
        }
    });
    let printer = (!quiet).then(|| tokio::spawn(print_progress(rx)));

    let result = idma_engine::download(&req, &opts, Some(&tx), &cancel).await;
    if let Some(p) = printer {
        p.abort();
        eprintln!();
    }
    match result {
        Ok(done) => {
            if !quiet {
                let secs = done.elapsed.as_secs_f64();
                eprintln!(
                    "done: {} in {secs:.1}s ({}/s){}",
                    human(done.size as f64),
                    human(done.size.saturating_sub(done.resumed_from) as f64 / secs.max(1e-3)),
                    if done.resumed_from > 0 {
                        format!(", resumed from {}", human(done.resumed_from as f64))
                    } else {
                        String::new()
                    }
                );
                if let Some(c) = &done.checksum {
                    eprintln!(
                        "checksum ok: {c}{}",
                        from_note(done.checksum_from.as_deref())
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(e @ Error::DiskFull { .. }) => {
            eprintln!("dm: {e}; free some space and run the same command again to resume");
            ExitCode::FAILURE
        }
        Err(Error::Cancelled) => {
            eprintln!("dm: cancelled; run the same command again to resume");
            ExitCode::from(130)
        }
        Err(e) => {
            eprintln!("dm: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn print_progress(mut rx: tokio::sync::watch::Receiver<Progress>) {
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tick.tick().await;
        if rx.has_changed().is_err() {
            return;
        }
        let p = rx.borrow_and_update().clone();
        if p.phase == Phase::Verifying {
            let pct = p
                .size
                .filter(|s| *s > 0)
                .map_or(0.0, |s| p.verified as f64 / s as f64 * 100.0);
            eprint!(
                "\rverifying{} {pct:>5.1}%{:<30}",
                from_note(p.checksum_from.as_deref()),
                ""
            );
            std::io::stderr().flush().ok();
            continue;
        }
        let pct = p
            .size
            .filter(|s| *s > 0)
            .map(|s| p.received as f64 / s as f64 * 100.0);
        let eta = match (p.size, p.speed > 0.0) {
            (Some(s), true) => format!("{:.0}s", s.saturating_sub(p.received) as f64 / p.speed),
            _ => "?".into(),
        };
        eprint!(
            "\r{:>6} {:>10} / {:<10} {:>10}/s  conns {:>2}  eta {:<6}",
            pct.map_or("".into(), |p| format!("{p:.1}%")),
            human(p.received as f64),
            p.size.map_or("?".into(), |s| human(s as f64)),
            human(p.speed),
            p.connections,
            eta
        );
        std::io::stderr().flush().ok();
    }
}

/// ` (checksum from SHA256SUMS)` for a checksum the user did not give.
fn from_note(from: Option<&str>) -> String {
    from.map(|f| format!(" (checksum from {f})"))
        .unwrap_or_default()
}

fn parse_browser(s: &str) -> Result<Option<Browser>, String> {
    if s.eq_ignore_ascii_case("none") {
        Ok(None)
    } else {
        s.parse().map(Some)
    }
}

fn parse_rate(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (digits, shift) = match s.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => (
            &s[..i],
            match c.to_ascii_uppercase() {
                'K' => 10,
                'M' => 20,
                'G' => 30,
                _ => return Err(format!("unknown unit `{c}` (use K, M or G)")),
            },
        ),
        _ => (s, 0),
    };
    let n: f64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("not a rate: `{s}`"))?;
    let rate = (n * f64::from(1u32 << shift)) as u64;
    if n.is_finite() && rate > 0 {
        Ok(rate)
    } else {
        Err(format!("rate must be positive: `{s}`"))
    }
}

fn human(b: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_valid() {
        Cli::command().debug_assert();
        let parse = |args: &[&str]| match Cli::try_parse_from(args).expect("parse").command {
            Command::Get { net, .. } => net.browser,
            Command::Zip { .. } => panic!("not get"),
        };
        assert_eq!(parse(&["dm", "get", "u", "-o", "f"]), Some(Browser::Chrome));
        assert_eq!(
            parse(&["dm", "get", "u", "-o", "f", "--browser", "none"]),
            None
        );
        assert_eq!(
            parse(&["dm", "get", "u", "-o", "f", "--browser", "Firefox"]),
            Some(Browser::Firefox)
        );
        let zip = Cli::try_parse_from(["dm", "zip", "get", "u", "a.txt", "d/**", "-d", "out"])
            .expect("zip get");
        let Command::Zip {
            command: ZipCommand::Get {
                entries, out_dir, ..
            },
        } = zip.command
        else {
            panic!("not zip get")
        };
        assert_eq!(entries, ["a.txt", "d/**"]);
        assert_eq!(out_dir, Some(PathBuf::from("out")));
        assert!(Cli::try_parse_from(["dm", "zip", "get", "u"]).is_err());
        assert!(Cli::try_parse_from(["dm", "zip", "list", "u", "-k"]).is_ok());
    }

    #[test]
    fn rates() {
        assert_eq!(parse_rate("1000"), Ok(1000));
        assert_eq!(parse_rate("64K"), Ok(64 << 10));
        assert_eq!(parse_rate("1.5m"), Ok(3 << 19));
        assert_eq!(parse_rate("2G"), Ok(2 << 30));
        assert!(parse_rate("0").is_err());
        assert!(parse_rate("5x").is_err());
        assert!(parse_rate("fast").is_err());
    }
}
