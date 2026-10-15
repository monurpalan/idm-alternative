use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use idma_engine::{CancellationToken, Error, Options, Progress, Request};

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
        /// Output file.
        #[arg(short, long)]
        output: PathBuf,
        /// Maximum simultaneous connections.
        #[arg(short, long, default_value_t = 16)]
        connections: usize,
        /// Extra request header, `Name: value` (repeatable).
        #[arg(short = 'H', long = "header")]
        headers: Vec<String>,
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
        connections,
        headers,
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

    let req = Request {
        url,
        output,
        headers,
    };
    let opts = Options {
        connections,
        ..Options::default()
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
