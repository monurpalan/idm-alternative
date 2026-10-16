use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use idma_engine::{
    Browser, CancellationToken, Checksum, Conflict, Error, Limiter, Options, Phase, Progress,
    Request,
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
        /// Verify the finished file: `sha256:<hex>` (also md5, sha1, sha512), or a bare hex
        /// digest.
        #[arg(long)]
        checksum: Option<Checksum>,
        /// Never use HTTP/2.
        #[arg(long)]
        http1: bool,
        /// No progress line.
        #[arg(short, long)]
        quiet: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("DM_LOG").unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let Command::Get {
        url,
        output,
        dir,
        overwrite,
        connections,
        limit,
        headers,
        browser,
        user_agent,
        proxy,
        insecure,
        http1,
        checksum,
        quiet,
    } = Cli::parse().command;
    let headers = match headers
        .iter()
        .map(|h| {
            h.split_once(':')
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                .ok_or_else(|| format!("header must be `Name: value`, got `{h}`"))
        })
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(h) => h,
        Err(e) => {
            eprintln!("dm: {e}");
            return ExitCode::from(2);
        }
    };

    let opts = Options {
        connections,
        limiters: limit.map(|r| Limiter::new(Some(r))).into_iter().collect(),
        browser,
        user_agent,
        proxy,
        insecure,
        http1_only: http1,
        checksum,
        ..Options::default()
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
            eprint!("\rverifying {pct:>5.1}%{:<60}", "");
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
            Command::Get { browser, .. } => browser,
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
