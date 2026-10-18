use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use shardload_testserver::scenario::Size;
use shardload_testserver::{SelfSigned, Verdict, router, serve_plain, serve_tls, verify_file};

#[derive(Parser)]
#[command(
    name = "testserver",
    about = "Hostile HTTP server for download-manager testing"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve test files (HTTP/1.1 and h2c, or HTTPS with HTTP/2 and HTTP/1.1).
    Serve {
        #[arg(long, default_value = "127.0.0.1:8787")]
        bind: SocketAddr,
        /// HTTPS with a fresh self-signed certificate (clients need `-k` or the PEM).
        #[arg(long)]
        tls: bool,
    },
    /// Check a downloaded file against the expected content.
    Verify {
        file: PathBuf,
        #[arg(long)]
        size: Size,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    match Cli::parse().command {
        Command::Serve { bind, tls } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .init();
            let listener = tokio::net::TcpListener::bind(bind).await?;
            if tls {
                let cert = SelfSigned::generate()?;
                tracing::info!("listening on https://{bind}/file/test.bin?size=100M");
                println!("{}", cert.pem);
                serve_tls(listener, router(), &cert).await?;
            } else {
                tracing::info!("listening on http://{bind}/file/test.bin?size=100M");
                serve_plain(listener, router()).await?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Verify { file, size, seed } => match verify_file(&file, seed, size.0)? {
            Verdict::Ok => {
                println!("OK: {} bytes match", size.0);
                Ok(ExitCode::SUCCESS)
            }
            Verdict::WrongLength { expected, actual } => {
                println!("FAIL: length {actual}, expected {expected}");
                Ok(ExitCode::FAILURE)
            }
            Verdict::Mismatch { offset } => {
                println!("FAIL: first wrong byte at offset {offset}");
                Ok(ExitCode::FAILURE)
            }
        },
    }
}
