use anyhow::Result;
use clap::Parser;
use runflow::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    cli.run().await
}
