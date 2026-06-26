mod cli;
mod config;
mod db;
mod helius;
mod ingest;
mod mcp;
mod model;
mod paper;
mod prune;
mod pumpfun_decode;
mod pumpportal;
mod screen;
mod stats;
mod survivors;
mod util;
mod writer;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    // rustls 0.23 ships multiple crypto providers; pick one explicitly so the
    // TLS handshake doesn't panic trying to auto-select.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    // All logging to STDERR — stdout is reserved for the MCP JSON-RPC stream.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .init();

    let cli = cli::Cli::parse();
    let config = config::Config::from_env()?;

    match cli.command {
        cli::Command::Ingest => ingest::run(config).await?,
        cli::Command::Mcp => mcp::run(&config.snapshot_path).await?,
        cli::Command::Prune => prune::run(&config)?,
        cli::Command::Stats => stats::run(&config)?,
        cli::Command::Screen { minutes, tier, limit } => {
            let p = screen::ScreenParams::from_args(Some(minutes), tier.as_deref(), Some(limit));
            screen::run(&config, p)?;
        }
        cli::Command::Papertrade { entry_secs, min_buyers, tp, sl, hold_secs } => {
            let p = paper::PaperParams::resolve(entry_secs, min_buyers, tp, sl, hold_secs);
            paper::run(config, p).await?;
        }
        cli::Command::Survivors { age_min, age_max, limit } => {
            let p = survivors::SurvivorParams::from_args(age_min, age_max, limit);
            survivors::run(&config, p)?;
        }
    }
    Ok(())
}
