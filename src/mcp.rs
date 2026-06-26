//! MCP (stdio) server exposing the read-only snapshot DuckDB to a Claude agent.
//!
//! IMPORTANT: stdout is the JSON-RPC channel. Nothing may be printed to stdout
//! here — all logging goes to stderr (configured in main). Queries run on the
//! blocking pool because DuckDB is synchronous and `!Sync`.

use anyhow::Result;
use duckdb::params;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

use crate::db::{self, ReaderPool};
use crate::util::now_ms;

#[derive(Clone)]
pub struct MemeServer {
    pool: ReaderPool,
    // Used by the #[tool_handler]/#[tool_router] generated dispatch; the
    // dead-code analyzer can't see through the macro.
    #[allow(dead_code)]
    tool_router: ToolRouter<MemeServer>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecentArgs {
    /// Look back this many minutes (default 30).
    pub minutes: Option<u32>,
    /// Max rows (default 50).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MintArgs {
    pub mint: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TopArgs {
    pub minutes: Option<u32>,
    /// One of: peak_mcap | trade_count | cap_growth (default peak_mcap).
    pub by: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WalletArgs {
    pub trader: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SqlArgs {
    /// A read-only SELECT. The connection is opened read-only, so writes fail.
    pub sql: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScreenArgs {
    /// Look back this many minutes (default 20).
    pub minutes: Option<u32>,
    /// Tier override: balanced (default) | gate60 | conviction60 | gate120 | inflow120 | sustained.
    pub tier: Option<String>,
    /// Max rows (default 30).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SurvivorArgs {
    /// Cohort lower age bound in minutes (default 30).
    pub age_min: Option<u32>,
    /// Cohort upper age bound in minutes (default 45).
    pub age_max: Option<u32>,
    /// Max rows (default 30).
    pub limit: Option<u32>,
}

#[tool_router]
impl MemeServer {
    pub fn new(pool: ReaderPool) -> Self {
        Self { pool, tool_router: Self::tool_router() }
    }

    #[tool(description = "New tokens created in the last N minutes (default 30).")]
    async fn query_recent_tokens(&self, Parameters(a): Parameters<RecentArgs>) -> Result<CallToolResult, McpError> {
        let cutoff = now_ms() - (a.minutes.unwrap_or(30) as i64) * 60_000;
        let limit = a.limit.unwrap_or(50) as i64;
        self.blocking_json(move |c| {
            db::query_json(
                c,
                "SELECT mint, symbol, name, creator, created_ms, market_cap_sol, initial_buy_sol
                 FROM new_tokens WHERE created_ms >= ? ORDER BY created_ms DESC LIMIT ?",
                params![cutoff, limit],
            )
        })
        .await
    }

    #[tool(description = "One token's row plus aggregate trade stats (counts, unique traders, peak/min market cap).")]
    async fn token_detail(&self, Parameters(a): Parameters<MintArgs>) -> Result<CallToolResult, McpError> {
        let mint = a.mint;
        self.blocking_json(move |c| {
            db::query_json(
                c,
                "SELECT t.*,
                   (SELECT count(*) FROM trades x WHERE x.mint=t.mint) AS trade_count,
                   (SELECT count(*) FROM trades x WHERE x.mint=t.mint AND side='buy') AS buys,
                   (SELECT count(*) FROM trades x WHERE x.mint=t.mint AND side='sell') AS sells,
                   (SELECT count(DISTINCT trader) FROM trades x WHERE x.mint=t.mint) AS unique_traders,
                   (SELECT min(ts_ms) FROM trades x WHERE x.mint=t.mint) AS first_trade_ms,
                   (SELECT max(ts_ms) FROM trades x WHERE x.mint=t.mint) AS last_trade_ms,
                   (SELECT max(market_cap_sol) FROM trades x WHERE x.mint=t.mint) AS peak_mcap,
                   (SELECT min(market_cap_sol) FROM trades x WHERE x.mint=t.mint) AS min_mcap
                 FROM new_tokens t WHERE t.mint = ?",
                params![mint],
            )
        })
        .await
    }

    #[tool(description = "Chronological trades for a mint (default 200).")]
    async fn token_trades(&self, Parameters(a): Parameters<MintArgs>) -> Result<CallToolResult, McpError> {
        let mint = a.mint;
        let limit = a.limit.unwrap_or(200) as i64;
        self.blocking_json(move |c| {
            db::query_json(
                c,
                "SELECT ts_ms, side, trader, sol_amount, token_amount, market_cap_sol, new_token_balance
                 FROM trades WHERE mint = ? ORDER BY ts_ms ASC LIMIT ?",
                params![mint, limit],
            )
        })
        .await
    }

    #[tool(description = "Top tokens in the last N minutes ranked by peak_mcap | trade_count | cap_growth.")]
    async fn top_tokens(&self, Parameters(a): Parameters<TopArgs>) -> Result<CallToolResult, McpError> {
        let cutoff = now_ms() - (a.minutes.unwrap_or(180) as i64) * 60_000;
        let limit = a.limit.unwrap_or(25) as i64;
        let order = match a.by.as_deref() {
            Some("trade_count") => "trade_count",
            Some("cap_growth") => "cap_growth",
            _ => "peak_mcap",
        };
        let sql = format!(
            "SELECT n.mint, n.symbol, n.created_ms,
               max(tr.market_cap_sol) AS peak_mcap,
               count(tr.mint) AS trade_count,
               max(tr.market_cap_sol)/nullif(min(tr.market_cap_sol),0) AS cap_growth
             FROM new_tokens n JOIN trades tr ON tr.mint = n.mint
             WHERE n.created_ms >= ?
             GROUP BY n.mint, n.symbol, n.created_ms
             ORDER BY {order} DESC NULLS LAST LIMIT ?"
        );
        self.blocking_json(move |c| db::query_json(c, &sql, params![cutoff, limit])).await
    }

    #[tool(description = "All trades by one wallet across mints (surfaces cross-token wallet behavior).")]
    async fn wallet_activity(&self, Parameters(a): Parameters<WalletArgs>) -> Result<CallToolResult, McpError> {
        let trader = a.trader;
        let limit = a.limit.unwrap_or(200) as i64;
        self.blocking_json(move |c| {
            db::query_json(
                c,
                "SELECT mint, ts_ms, side, sol_amount, market_cap_sol
                 FROM trades WHERE trader = ? ORDER BY ts_ms DESC LIMIT ?",
                params![trader, limit],
            )
        })
        .await
    }

    #[tool(description = "Window health: token/trade counts, time span, distinct traders.")]
    async fn window_stats(&self) -> Result<CallToolResult, McpError> {
        self.blocking_json(|c| {
            db::query_json(
                c,
                "SELECT
                   (SELECT count(*) FROM new_tokens) AS tokens,
                   (SELECT count(*) FROM trades) AS trades,
                   (SELECT min(ts_ms) FROM trades) AS first_trade_ms,
                   (SELECT max(ts_ms) FROM trades) AS last_trade_ms,
                   (SELECT count(DISTINCT trader) FROM trades) AS distinct_traders",
                params![],
            )
        })
        .await
    }

    #[tool(description = "Run a read-only SELECT over `new_tokens` and `trades` (time columns are epoch-millis: created_ms, ts_ms). A LIMIT is enforced.")]
    async fn run_readonly_sql(&self, Parameters(a): Parameters<SqlArgs>) -> Result<CallToolResult, McpError> {
        let limit = a.limit.unwrap_or(200).min(5000) as i64;
        let inner = a.sql.trim().trim_end_matches(';').to_string();
        let sql = format!("SELECT * FROM ({inner}) AS q LIMIT {limit}");
        self.blocking_json(move |c| db::query_json(c, &sql, params![])).await
    }

    #[tool(
        description = "Screen recent early-life pump.fun tokens against the learned heuristics. \
        Default tier = balanced/high-conviction (>=20 distinct buyers, >=2 SOL net inflow, no whale >50% in the first 2 min); \
        kill-filters applied; ranked by net inflow. Only scores tokens whose 120s window has closed within the data horizon \
        (uses max(ts_ms) as the clock, not wall-clock). Each row carries the features + a `sustained` flag + `reasons`. \
        Args: minutes (default 20), tier (balanced|gate60|conviction60|gate120|inflow120|sustained|all), limit (default 30). \
        Use tier=all to get the unfiltered feature rows for EVERY recent token (closed 120s window) and apply the latest SKILL.md patterns yourself — SKILL.md is the source of truth."
    )]
    async fn screen_candidates(&self, Parameters(a): Parameters<ScreenArgs>) -> Result<CallToolResult, McpError> {
        let p = crate::screen::ScreenParams::from_args(a.minutes, a.tier.as_deref(), a.limit);
        self.blocking_json(move |c| crate::screen::screen(c, &p)).await
    }

    #[tool(
        description = "THE 40-minute buy-decision screener (see SKILL.md '40-Minute Survivor Screening'). \
        For coins ~30-45 min old, answers 'is this a good buy now?'. Recomputes the smart-money wallet set \
        (wallets that buy CHEAP at launch — ≤35 SOL, first 60s — and win, validated to transfer cross-day at \
        ~2.4x base) from the rolling window, then surfaces cohort tokens whose first-60s buyers include those \
        smart wallets and/or that are still actively traded (fresh buyers in the last 10 min, ~3x lift). \
        Each row: age_min, smart_early_buyers, buyers_10m, trades_10m, cur_mcap, peak_mcap, last_trade_age_s, reasons. \
        Args: age_min (default 30), age_max (default 45), limit (default 30). Rare by design — good survivors are ~4% of launches."
    )]
    async fn screen_survivors(&self, Parameters(a): Parameters<SurvivorArgs>) -> Result<CallToolResult, McpError> {
        let p = crate::survivors::SurvivorParams::from_args(a.age_min, a.age_max, a.limit);
        self.blocking_json(move |c| crate::survivors::screen(c, &p)).await
    }

    async fn blocking_json<F>(&self, f: F) -> Result<CallToolResult, McpError>
    where
        F: FnOnce(&duckdb::Connection) -> Result<Value> + Send + 'static,
    {
        let pool = self.pool.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<Value> {
            let conn = pool.get()?;
            f(&conn)
        })
        .await;
        match result {
            Ok(Ok(v)) => Ok(CallToolResult::success(vec![Content::text(v.to_string())])),
            Ok(Err(e)) => Err(McpError::internal_error(e.to_string(), None)),
            Err(e) => Err(McpError::internal_error(e.to_string(), None)),
        }
    }
}

#[tool_handler]
impl ServerHandler for MemeServer {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo is #[non_exhaustive]: start from default, then set fields.
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Read-only access to the last ~24h of Solana memecoin launches and trades \
             (PumpPortal). Tables: new_tokens, trades. Time columns are epoch-millis \
             (created_ms, ts_ms). Use the tools to discover patterns that separate \
             tokens that pumped from those that rugged."
                .to_string(),
        );
        info
    }
}

pub async fn run(snapshot_path: &Path) -> Result<()> {
    if !snapshot_path.exists() {
        anyhow::bail!(
            "snapshot db {} does not exist yet — run the ingester first (it writes the snapshot every few minutes)",
            snapshot_path.display()
        );
    }
    let pool = db::reader_pool(snapshot_path, 4)?;
    let server = MemeServer::new(pool);
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
