use clap::Parser;

/// Command-line front end for the idm-alternative download engine.
#[derive(Parser)]
#[command(name = "dm", version = idma_engine::VERSION)]
struct Cli {}

fn main() -> anyhow::Result<()> {
    let _cli = Cli::parse();
    println!(
        "dm {} — engine not implemented yet",
        idma_engine::VERSION
    );
    Ok(())
}
