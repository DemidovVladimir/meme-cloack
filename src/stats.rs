//! `stats` subcommand: quick human ops check. Reads the read-only snapshot (so it
//! works while the ingester holds the hot DB), falling back to the hot file if no
//! snapshot exists yet and the ingester is not running.

use anyhow::Result;
use std::path::Path;

use crate::config::Config;
use crate::db;

pub fn run(config: &Config) -> Result<()> {
    let path: &Path = if config.snapshot_path.exists() {
        &config.snapshot_path
    } else {
        &config.db_path
    };
    if !path.exists() {
        println!("no database yet at {} (start the ingester)", path.display());
        return Ok(());
    }

    let conn = db::open_reader(path)?;
    let summary = db::query_json(
        &conn,
        "SELECT
           (SELECT count(*) FROM new_tokens) AS tokens,
           (SELECT count(*) FROM trades) AS trades,
           (SELECT count(DISTINCT trader) FROM trades) AS distinct_traders,
           (SELECT min(created_ms) FROM new_tokens) AS oldest_token_ms,
           (SELECT max(ts_ms) FROM trades) AS last_trade_ms",
        duckdb::params![],
    )?;
    println!("source: {}", path.display());
    println!("{}", serde_json::to_string_pretty(&summary)?);

    let top = db::query_json(
        &conn,
        "SELECT n.symbol, n.mint, count(tr.mint) AS trades, max(tr.market_cap_sol) AS peak_mcap
         FROM new_tokens n JOIN trades tr ON tr.mint = n.mint
         GROUP BY n.symbol, n.mint ORDER BY trades DESC LIMIT 10",
        duckdb::params![],
    )?;
    println!("top tokens by trade count:\n{}", serde_json::to_string_pretty(&top)?);
    Ok(())
}
