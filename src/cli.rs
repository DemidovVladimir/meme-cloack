use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "meme-expert", version, about = "Lean Solana memecoin ingester + MCP over a rolling 24h DuckDB")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the PumpPortal ingester (new tokens + survivor trade capture, rolling 24h).
    Ingest,
    /// Run the MCP stdio server over the read-only snapshot (for Claude).
    Mcp,
    /// Manual retention prune + snapshot rebuild (run only while the ingester is stopped).
    Prune,
    /// Print database stats (reads the read-only snapshot).
    Stats,
}
