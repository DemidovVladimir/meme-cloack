use anyhow::Result;
use std::path::PathBuf;

/// Runtime configuration, loaded from environment (.env is loaded in main).
#[derive(Clone, Debug)]
pub struct Config {
    pub pumpportal_api_key: Option<String>,
    pub pumpportal_ws_url: String,
    pub db_path: PathBuf,
    pub snapshot_path: PathBuf,

    // --- ingestion source selection ---
    pub ingest_source: IngestSource,
    pub helius_api_key: Option<String>,
    pub helius_ws_url: String,
    pub pump_program_id: String,
    /// If > 0, only persist each token's first N minutes of trades (live-screener
    /// storage bound). 0 = keep all trades within the retention window (best for
    /// re-training, which needs full outcomes — peak / lifespan / survival).
    pub early_trade_window_minutes: f64,

    pub retention_hours: f64,

    // --- 40-minute survivor / early-rug tracking policy ---
    /// A token that reaches this age without dying is a "survivor": keep tracking
    /// it through the retention window. Before this age, the death rules apply.
    pub survivor_age_minutes: f64,
    /// Death by collapse: latest market cap below this fraction of its running peak.
    pub death_drawdown_pct: f64,
    /// Death by silence: no trades for this many minutes (while still pre-survivor).
    pub death_silence_minutes: f64,

    /// Hard cap on concurrently trade-subscribed mints (cost back-pressure).
    pub max_active_trade_subs: usize,

    pub prune_interval_minutes: f64,
    pub snapshot_interval_minutes: f64,
    pub writer_flush_ms: u64,
    pub writer_flush_rows: usize,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let api_key = env_opt("PUMPPORTAL_API_KEY");
        let base = env_str("PUMPPORTAL_WS_URL", "wss://pumpportal.fun/api/data");
        let ws_url = match &api_key {
            Some(key) if !key.is_empty() => {
                let sep = if base.contains('?') { '&' } else { '?' };
                format!("{base}{sep}api-key={key}")
            }
            _ => base,
        };

        // Helius LaserStream WebSocket: full URL override, else build from key.
        let helius_api_key = env_opt("HELIUS_API_KEY");
        let helius_ws_url = match env_opt("HELIUS_WS_URL") {
            Some(u) => u,
            None => {
                let hbase = env_str("HELIUS_WS_BASE", "wss://mainnet.helius-rpc.com");
                match &helius_api_key {
                    Some(k) if !k.is_empty() => {
                        // Ensure a "/" path before the query — Helius returns HTTP 400
                        // on `host?query` (needs `host/?query`).
                        let base = if hbase.contains('?') {
                            hbase
                        } else {
                            format!("{}/", hbase.trim_end_matches('/'))
                        };
                        let sep = if base.contains('?') { '&' } else { '?' };
                        format!("{base}{sep}api-key={k}")
                    }
                    _ => hbase,
                }
            }
        };
        let ingest_source = match env_str("INGEST_SOURCE", "pumpportal").to_ascii_lowercase().as_str() {
            "helius" | "helius_ws" | "heliusws" | "ws" => IngestSource::HeliusWs,
            _ => IngestSource::PumpPortal,
        };

        Ok(Self {
            pumpportal_api_key: api_key,
            pumpportal_ws_url: ws_url,
            db_path: env_str("MEME_DB_PATH", "./data/hot.duckdb").into(),
            snapshot_path: env_str("MEME_SNAPSHOT_PATH", "./data/snapshot.duckdb").into(),
            ingest_source,
            helius_api_key,
            helius_ws_url,
            pump_program_id: env_str("PUMP_PROGRAM_ID", "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"),
            early_trade_window_minutes: env_f64("EARLY_TRADE_WINDOW_MINUTES", 0.0),
            retention_hours: env_f64("RETENTION_HOURS", 24.0),
            survivor_age_minutes: env_f64("SURVIVOR_AGE_MINUTES", 40.0),
            death_drawdown_pct: env_f64("DEATH_DRAWDOWN_PCT", 0.25),
            death_silence_minutes: env_f64("DEATH_SILENCE_MINUTES", 5.0),
            max_active_trade_subs: env_usize("MAX_ACTIVE_TRADE_SUBS", 500),
            prune_interval_minutes: env_f64("PRUNE_INTERVAL_MINUTES", 5.0),
            snapshot_interval_minutes: env_f64("SNAPSHOT_INTERVAL_MINUTES", 5.0),
            writer_flush_ms: env_u64("WRITER_FLUSH_MS", 1000),
            writer_flush_rows: env_usize("WRITER_FLUSH_ROWS", 500),
        })
    }

    pub fn retention_ms(&self) -> i64 {
        (self.retention_hours * 3_600_000.0) as i64
    }
    pub fn survivor_age_ms(&self) -> i64 {
        (self.survivor_age_minutes * 60_000.0) as i64
    }
    pub fn death_silence_ms(&self) -> i64 {
        (self.death_silence_minutes * 60_000.0) as i64
    }
    /// Some(ms) if a positive early-trade window is configured; None = keep all
    /// trades within the retention window.
    pub fn early_trade_window_ms(&self) -> Option<i64> {
        if self.early_trade_window_minutes > 0.0 {
            Some((self.early_trade_window_minutes * 60_000.0) as i64)
        } else {
            None
        }
    }
}

/// Which live source fills the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IngestSource {
    /// PumpPortal pre-decoded WS (per-token subs, metered) — fallback.
    PumpPortal,
    /// Helius LaserStream WebSocket `transactionSubscribe` on the pump.fun program
    /// (complete firehose, no sub cap) — default for the screener.
    HeliusWs,
}

fn env_opt(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

fn env_str(key: &str, default: &str) -> String {
    env_opt(key).unwrap_or_else(|| default.to_string())
}

fn env_f64(key: &str, default: f64) -> f64 {
    env_opt(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    env_opt(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    env_opt(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}
