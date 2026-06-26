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
    /// Screen recent early-life tokens against the heuristics (reads the snapshot).
    Screen {
        /// Look back this many minutes.
        #[arg(long, default_value_t = 20)]
        minutes: u32,
        /// Tier: balanced|gate60|conviction60|gate120|inflow120|sustained.
        #[arg(long)]
        tier: Option<String>,
        /// Max rows.
        #[arg(long, default_value_t = 30)]
        limit: u32,
    },
    /// Paper-trade the live high-conviction signal (SIMULATED — no real money, no keys).
    /// Opens its own Helius stream; logs fee+slippage-adjusted P&L to a JSONL file.
    Papertrade {
        /// Decision age in seconds (when the entry is evaluated).
        #[arg(long)]
        entry_secs: Option<u64>,
        /// Min distinct early buyers required to enter.
        #[arg(long)]
        min_buyers: Option<usize>,
        /// Take-profit fraction (e.g. 0.5 = +50%).
        #[arg(long)]
        tp: Option<f64>,
        /// Stop-loss fraction (e.g. 0.3 = -30%).
        #[arg(long)]
        sl: Option<f64>,
        /// Max hold seconds before a timeout exit.
        #[arg(long)]
        hold_secs: Option<u64>,
    },
    /// Screen ~30-45 min old "survivor" tokens as live BUY candidates: smart-money
    /// early buyers + still-active. The 40-minute buy-decision tool (reads the snapshot).
    Survivors {
        /// Cohort lower age bound, minutes (default 30).
        #[arg(long)]
        age_min: Option<u32>,
        /// Cohort upper age bound, minutes (default 45).
        #[arg(long)]
        age_max: Option<u32>,
        /// Max rows (default 30).
        #[arg(long)]
        limit: Option<u32>,
    },
}
