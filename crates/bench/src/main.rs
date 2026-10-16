//! Benchmark runner: starts the test server in-process, runs each download client on each
//! scenario, samples the server and the client process while it runs, verifies the result,
//! and writes a Markdown summary plus raw JSON.

mod config;
mod report;
mod run;

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "bench",
    about = "Compare download managers on test-server scenarios"
)]
struct Cli {
    /// Benchmark configuration.
    #[arg(long, default_value = "bench/bench.toml")]
    config: PathBuf,
    /// Where clients write downloads (cleaned after each run). Use a fast disk with free space.
    #[arg(long)]
    out_dir: Option<PathBuf>,
    /// Where summaries are written.
    #[arg(long, default_value = "bench/results")]
    results_dir: PathBuf,
    /// Only run scenarios whose name contains this text (also enables disabled ones).
    #[arg(long)]
    scenario: Option<String>,
    /// Only run these clients (comma-separated; a name that is not a client's exact name
    /// selects every client containing it). Also enables disabled ones, e.g. `--client "dm,IDM"`.
    #[arg(long)]
    client: Option<String>,
    /// Override the repeat count from the config.
    #[arg(long)]
    repeat: Option<u32>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = config::load(&cli.config)?;

    let scenarios: Vec<_> = cfg
        .scenario
        .iter()
        .filter(|s| match &cli.scenario {
            Some(f) => s.name.contains(f.as_str()),
            None => s.enabled,
        })
        .collect();
    let clients: Vec<_> = cfg
        .client
        .iter()
        .filter(|c| match &cli.client {
            Some(f) => {
                let names: Vec<&str> = f.split(',').map(str::trim).collect();
                names.iter().any(|n| {
                    if clients_named(&cfg.client, n) {
                        c.name == *n
                    } else {
                        c.name.contains(n)
                    }
                })
            }
            None => c.enabled,
        })
        .collect();
    anyhow::ensure!(!scenarios.is_empty(), "no scenario matches");
    anyhow::ensure!(!clients.is_empty(), "no client matches");
    let repeat = cli.repeat.unwrap_or(cfg.repeat).max(1);

    let out_dir = cli
        .out_dir
        .unwrap_or_else(|| std::env::temp_dir().join("idma-bench"));
    std::fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    tokio::spawn(async move { axum::serve(listener, idma_testserver::router()).await });

    let mut results = Vec::new();
    let total = scenarios.len() * clients.len() * repeat as usize;
    let mut n = 0;
    let mut order: Vec<usize> = (0..clients.len()).collect();
    let mut rng = seed_from_clock();
    for scenario in &scenarios {
        // Internet runs: the first successful download's hash, which the others must match.
        let mut reference = None;
        for k in 1..=repeat {
            // A different client order every round, so time-of-day effects (network load,
            // disk write-back) do not always land on the same client.
            shuffle(&mut order, &mut rng);
            for &i in &order {
                let client = clients[i];
                n += 1;
                eprint!(
                    "[{n}/{total}] {} · {} · run {k} … ",
                    scenario.name, client.name
                );
                let r = run::run_once(
                    &base,
                    scenario,
                    client,
                    k,
                    &out_dir,
                    cfg.timeout_secs,
                    &mut reference,
                )
                .await;
                eprintln!("{}", r.one_line());
                results.push(r);
            }
        }
    }

    let stamp = chrono::Local::now().format("%Y-%m-%d_%H%M%S").to_string();
    let md = report::markdown(&scenarios, &clients, &results, repeat);
    println!("\n{md}");

    std::fs::create_dir_all(cli.results_dir.join("raw"))?;
    let md_path = cli.results_dir.join(format!("{stamp}.md"));
    let raw_path = cli.results_dir.join("raw").join(format!("{stamp}.json"));
    std::fs::write(&md_path, &md)?;
    std::fs::write(&raw_path, serde_json::to_string_pretty(&results)?)?;
    eprintln!("wrote {} and {}", md_path.display(), raw_path.display());
    Ok(())
}

fn seed_from_clock() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9e37_79b9, |d| d.as_nanos() as u64)
        | 1
}

/// Fisher–Yates with xorshift64: good enough to vary the order, no dependency needed.
fn shuffle(v: &mut [usize], state: &mut u64) {
    for i in (1..v.len()).rev() {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        v.swap(i, (*state % (i as u64 + 1)) as usize);
    }
}

/// Whether some client is called exactly `name`; then `name` selects only that one.
fn clients_named(clients: &[config::Client], name: &str) -> bool {
    clients.iter().any(|c| c.name == name)
}
