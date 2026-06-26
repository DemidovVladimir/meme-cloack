//! 40-minute "survivor" screener — the live buy-decision tool.
//!
//! Question it answers: *a coin is ~30–45 min old — is it a good buy NOW?*
//! Two validated signals (see SKILL.md "40-Minute Survivor Screening"):
//!   1. **Smart money** — did current cheap-launch-winner wallets buy it in its
//!      first 60s? The smart set is recomputed from the rolling window on every
//!      call (the set decays ~⅓/day, so a stale list is never used).
//!   2. **Still active** — is it still pulling fresh buyers in the last ~10 min?
//! Pure SQL over the read-only snapshot; source-agnostic.

use anyhow::Result;
use serde_json::{json, Value};

use crate::config::Config;
use crate::db;

#[derive(Clone, Debug)]
pub struct SurvivorParams {
    /// Cohort = tokens created between `age_max` and `age_min` minutes ago.
    pub age_min: u32,
    pub age_max: u32,
    /// Smart-wallet set: min cheap-launch (≤35 SOL, first 60s) buys and min success %.
    pub smart_min_buys: u32,
    pub smart_min_succ: f64,
    /// Selectivity cap — exclude spray bots that trade more than this many distinct tokens.
    pub smart_max_tokens: u32,
    /// "Still active" threshold: distinct buyers in the last 10 min.
    pub active_min: u32,
    pub limit: u32,
}

impl SurvivorParams {
    pub fn from_args(age_min: Option<u32>, age_max: Option<u32>, limit: Option<u32>) -> Self {
        let age_min = age_min.unwrap_or(30).clamp(1, 1439);
        let age_max = age_max.unwrap_or(45).clamp(age_min + 1, 1440);
        SurvivorParams {
            age_min,
            age_max,
            smart_min_buys: 10,
            smart_min_succ: 40.0,
            smart_max_tokens: 200,
            active_min: 10,
            limit: limit.unwrap_or(30).clamp(1, 200),
        }
    }
}

/// The full screener query. Computes the smart-wallet set fresh, then scores the
/// 30–45-min cohort by smart-early-buyers + recent activity.
pub fn build_sql(p: &SurvivorParams) -> String {
    format!(
        r#"
WITH horizon AS (SELECT max(ts_ms) AS t FROM trades),
tok_all AS (
  SELECT n.mint, n.created_ms, n.creator, max(tr.market_cap_sol) AS peak
  FROM new_tokens n JOIN trades tr ON tr.mint = n.mint
  GROUP BY n.mint, n.created_ms, n.creator
),
-- total distinct coins each wallet traded — the spray-bot tell.
vol AS (SELECT trader, count(DISTINCT mint) AS total_tokens FROM trades GROUP BY trader),
clb AS (
  SELECT DISTINCT t.trader, t.mint, ta.peak
  FROM trades t JOIN tok_all ta ON ta.mint = t.mint
  WHERE t.side = 'buy' AND t.ts_ms <= ta.created_ms + 60000 AND t.market_cap_sol <= 35
    AND t.trader <> ta.creator   -- exclude the dev's own bundle-buy
),
-- "smart" = SELECTIVE winners: high cheap-launch hit rate AND not a spray bot.
smart AS (
  SELECT c.trader FROM clb c JOIN vol v ON v.trader = c.trader
  GROUP BY c.trader, v.total_tokens
  HAVING count(*) >= {smart_min_buys}
     AND 100.0 * count(*) FILTER (WHERE c.peak >= 84) / count(*) >= {smart_min_succ}
     AND v.total_tokens <= {smart_max_tokens}
),
cohort AS (
  SELECT n.mint, n.symbol, n.created_ms FROM new_tokens n CROSS JOIN horizon
  WHERE n.created_ms BETWEEN horizon.t - {age_max}*60000 AND horizon.t - {age_min}*60000
),
sw AS (
  SELECT c.mint, count(DISTINCT t.trader) AS smart_buyers
  FROM cohort c JOIN trades t ON t.mint = c.mint JOIN smart s ON s.trader = t.trader
  WHERE t.side = 'buy' AND t.ts_ms <= c.created_ms + 60000
  GROUP BY c.mint
),
act AS (
  SELECT c.mint,
    count(DISTINCT t.trader) FILTER (WHERE t.ts_ms >= (SELECT t FROM horizon) - 600000) AS buyers_10m,
    count(*) FILTER (WHERE t.ts_ms >= (SELECT t FROM horizon) - 600000) AS trades_10m,
    arg_max(t.market_cap_sol, t.ts_ms) AS cur_mcap,
    max(t.market_cap_sol) AS peak_mcap,
    max(t.ts_ms) AS last_ts
  FROM cohort c JOIN trades t ON t.mint = c.mint
  GROUP BY c.mint
)
SELECT c.symbol, c.mint,
  CAST(((SELECT t FROM horizon) - c.created_ms) / 60000 AS INT) AS age_min,
  COALESCE(sw.smart_buyers, 0) AS smart_early_buyers,
  COALESCE(act.buyers_10m, 0) AS buyers_10m,
  COALESCE(act.trades_10m, 0) AS trades_10m,
  ROUND(act.cur_mcap, 1) AS cur_mcap,
  ROUND(act.peak_mcap, 1) AS peak_mcap,
  CAST(((SELECT t FROM horizon) - act.last_ts) / 1000 AS INT) AS last_trade_age_s
FROM cohort c
LEFT JOIN sw ON sw.mint = c.mint
LEFT JOIN act ON act.mint = c.mint
WHERE COALESCE(sw.smart_buyers, 0) >= 1 OR COALESCE(act.buyers_10m, 0) >= {active_min}
ORDER BY COALESCE(sw.smart_buyers, 0) DESC, COALESCE(act.buyers_10m, 0) DESC
LIMIT {limit}
"#,
        smart_min_buys = p.smart_min_buys,
        smart_min_succ = p.smart_min_succ,
        smart_max_tokens = p.smart_max_tokens,
        age_max = p.age_max,
        age_min = p.age_min,
        active_min = p.active_min,
        limit = p.limit,
    )
}

/// Run the screen and enrich each row with human `reasons`.
pub fn screen(conn: &duckdb::Connection, p: &SurvivorParams) -> Result<Value> {
    let rows = db::query_json(conn, &build_sql(p), duckdb::params![])?;
    let Value::Array(rows) = rows else {
        return Ok(rows);
    };
    let enriched: Vec<Value> = rows
        .into_iter()
        .map(|mut r| {
            if let Value::Object(o) = &mut r {
                o.insert("reasons".into(), json!(reasons(o)));
            }
            r
        })
        .collect();
    Ok(Value::Array(enriched))
}

fn num(o: &serde_json::Map<String, Value>, k: &str) -> f64 {
    o.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

fn reasons(o: &serde_json::Map<String, Value>) -> Vec<String> {
    let mut r = Vec::new();
    let sb = num(o, "smart_early_buyers") as i64;
    if sb > 0 {
        r.push(format!("{sb} smart-money wallet(s) bought it early (cheap-launch winners)"));
    }
    let b = num(o, "buyers_10m") as i64;
    r.push(format!("{b} fresh buyers in the last 10 min"));
    if b >= 15 {
        r.push("still strongly active".into());
    } else if b < 5 {
        r.push("WARNING: activity fading".into());
    }
    let cur = num(o, "cur_mcap");
    let peak = num(o, "peak_mcap");
    if peak > 0.0 {
        r.push(format!("now ~{cur:.0} SOL mcap (peak ~{peak:.0})"));
    }
    r
}

/// CLI entry: screen the read-only snapshot and print JSON to stdout.
pub fn run(config: &Config, p: SurvivorParams) -> Result<()> {
    let conn = db::open_reader(&config.snapshot_path)?;
    let out = screen(&conn, &p)?;
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
