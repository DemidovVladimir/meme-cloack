//! Live early-life candidate screener.
//!
//! Source-agnostic: pure SQL over `new_tokens` + `trades` (whatever filled them).
//! It computes each recent token's first-120s microstructure and applies the
//! tiered heuristics derived from the frozen-data analysis. Two correctness
//! invariants:
//!   * **as-of clock** = `max(ts_ms)` in the data, NOT wall-clock — only tokens
//!     whose 120s window has closed within the data horizon are scored, so we
//!     never judge an incomplete window.
//!   * **signature dedup** — replay/overlap duplicates don't inflate counts.

use anyhow::Result;
use serde_json::{json, Value};

use crate::config::Config;
use crate::db;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// High-conviction: predicts a ≥3× peak (~63% precision in backtest).
    Balanced,
    Gate60,
    Conviction60,
    Gate120,
    Inflow120,
    /// Predicts a ≥3× peak that lasts ≥10 min (most "exitable").
    Sustained,
    /// No tier filter: return computed features for every recent token (with a
    /// closed 120s window) so the agent applies the LATEST SKILL.md patterns itself.
    All,
}

impl Tier {
    pub fn parse(s: &str) -> Tier {
        match s.trim().to_ascii_lowercase().as_str() {
            "gate60" | "gate_60" => Tier::Gate60,
            "conviction60" | "conviction_60" => Tier::Conviction60,
            "gate120" | "gate_120" => Tier::Gate120,
            "inflow120" | "inflow" => Tier::Inflow120,
            "sustained" | "sustain" => Tier::Sustained,
            "all" | "features" | "raw" => Tier::All,
            _ => Tier::Balanced,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Tier::Balanced => "high-conviction",
            Tier::Gate60 => "gate-60s",
            Tier::Conviction60 => "conviction-60s",
            Tier::Gate120 => "gate-120s",
            Tier::Inflow120 => "inflow-120s",
            Tier::Sustained => "sustained",
            Tier::All => "all",
        }
    }
    /// SQL predicate over the `feat` CTE columns.
    fn predicate(self) -> &'static str {
        match self {
            Tier::Balanced => "buyers_2m >= 20 AND net_2m >= 2 AND top_buy_share < 0.5",
            Tier::Gate60 => "buyers_60s >= 8",
            Tier::Conviction60 => "buyers_60s >= 12 AND net_60s > 0",
            Tier::Gate120 => "buyers_2m >= 10",
            Tier::Inflow120 => "buyers_2m >= 10 AND net_2m > 0",
            Tier::Sustained => "net_2m >= 10 AND buyers_min2 >= 30",
            Tier::All => "1=1",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScreenParams {
    pub minutes: u32,
    pub tier: Tier,
    pub limit: u32,
    pub apply_kill: bool,
}

impl ScreenParams {
    pub fn from_args(minutes: Option<u32>, tier: Option<&str>, limit: Option<u32>) -> Self {
        let tier = tier.map(Tier::parse).unwrap_or(Tier::Balanced);
        ScreenParams {
            minutes: minutes.unwrap_or(20).clamp(1, 1440),
            tier,
            // `all` returns every recent token's features unfiltered (so the agent
            // applies SKILL.md); the named tiers also apply the committed kill-filters.
            limit: limit.unwrap_or(30).clamp(1, 1000),
            apply_kill: tier != Tier::All,
        }
    }
}

/// Build the single feature+scoring query.
pub fn build_sql(p: &ScreenParams) -> String {
    let kill = if p.apply_kill {
        "AND NOT ((f.net_2m < 0 AND f.buyers_2m < 5) OR (COALESCE(sf.serial_frac,0) > 0.5 AND f.buyers_2m < 10))"
    } else {
        ""
    };
    format!(
        r#"
WITH horizon AS (SELECT max(ts_ms) AS t FROM trades),
cand AS (
  SELECT n.mint, n.symbol, n.created_ms
  FROM new_tokens n CROSS JOIN horizon
  WHERE n.created_ms >= horizon.t - {minutes}*60000
    AND n.created_ms + 120000 <= horizon.t
),
et AS (
  SELECT mint, trader, side, sol_amount, age_ms FROM (
    SELECT t.mint, t.trader, t.side, t.sol_amount, t.signature,
           (t.ts_ms - c.created_ms) AS age_ms,
           row_number() OVER (PARTITION BY t.signature ORDER BY t.ts_ms) AS rn
    FROM trades t JOIN cand c ON c.mint = t.mint
    WHERE t.ts_ms >= c.created_ms AND t.ts_ms <= c.created_ms + 120000
  ) WHERE rn = 1 OR signature IS NULL
),
serials AS (SELECT trader FROM trades GROUP BY trader HAVING count(DISTINCT mint) >= 20),
buyagg AS (
  SELECT mint, trader, SUM(CASE WHEN side='buy' THEN sol_amount ELSE 0 END) AS buy_sol
  FROM et GROUP BY mint, trader
),
topbuy AS (SELECT mint, MAX(buy_sol) AS max_buy, SUM(buy_sol) AS tot_buy FROM buyagg GROUP BY mint),
sf AS (
  SELECT e.mint,
    COUNT(DISTINCT CASE WHEN s.trader IS NOT NULL THEN e.trader END) * 1.0
      / NULLIF(COUNT(DISTINCT e.trader), 0) AS serial_frac
  FROM et e LEFT JOIN serials s ON s.trader = e.trader
  GROUP BY e.mint
),
feat AS (
  SELECT c.mint, c.symbol, c.created_ms,
    COUNT(DISTINCT CASE WHEN e.age_ms <= 60000 THEN e.trader END) AS buyers_60s,
    COUNT(DISTINCT e.trader) AS buyers_2m,
    COUNT(DISTINCT CASE WHEN e.age_ms > 60000 AND e.age_ms <= 120000 THEN e.trader END) AS buyers_min2,
    COALESCE(SUM(CASE WHEN e.age_ms <= 60000 THEN (CASE WHEN e.side='buy' THEN e.sol_amount ELSE -e.sol_amount END) ELSE 0 END), 0) AS net_60s,
    COALESCE(SUM(CASE WHEN e.side='buy' THEN e.sol_amount ELSE -e.sol_amount END), 0) AS net_2m
  FROM cand c LEFT JOIN et e ON e.mint = c.mint
  GROUP BY c.mint, c.symbol, c.created_ms
)
SELECT f.symbol, f.mint,
  CAST((a.t - f.created_ms) / 1000 AS BIGINT) AS age_s,
  f.buyers_60s, f.buyers_2m, f.buyers_min2,
  ROUND(f.net_60s, 3) AS net_60s, ROUND(f.net_2m, 3) AS net_2m,
  ROUND(COALESCE(tb.max_buy / NULLIF(tb.tot_buy, 0), 0), 3) AS top_buy_share,
  ROUND(COALESCE(sf.serial_frac, 0), 3) AS serial_frac,
  (f.net_2m >= 10 AND f.buyers_min2 >= 30) AS sustained
FROM feat f
CROSS JOIN horizon a
LEFT JOIN topbuy tb ON tb.mint = f.mint
LEFT JOIN sf ON sf.mint = f.mint
WHERE ({pred}) {kill}
ORDER BY f.net_2m DESC, f.buyers_2m DESC
LIMIT {limit}
"#,
        minutes = p.minutes,
        pred = p.tier.predicate(),
        kill = kill,
        limit = p.limit,
    )
}

/// Run the screen and enrich each row with `tier` + `reasons`.
pub fn screen(conn: &duckdb::Connection, p: &ScreenParams) -> Result<Value> {
    let sql = build_sql(p);
    let rows = db::query_json(conn, &sql, duckdb::params![])?;
    let Value::Array(rows) = rows else {
        return Ok(rows);
    };
    let enriched: Vec<Value> = rows
        .into_iter()
        .map(|mut r| {
            if let Value::Object(o) = &mut r {
                o.insert("tier".into(), json!(p.tier.label()));
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
    r.push(format!("{} distinct buyers in 2m", num(o, "buyers_2m") as i64));
    r.push(format!("net {:+.2} SOL in 2m", num(o, "net_2m")));
    let tbs = num(o, "top_buy_share");
    if tbs > 0.0 {
        r.push(format!("top buyer {:.0}% of buy vol", tbs * 100.0));
    }
    if num(o, "buyers_min2") > 0.0 {
        r.push(format!("{} buyers in the 2nd minute", num(o, "buyers_min2") as i64));
    }
    if o.get("sustained").and_then(Value::as_bool).unwrap_or(false) {
        r.push("SUSTAINED: big inflow + buying continues past 60s".into());
    }
    let sf = num(o, "serial_frac");
    if sf > 0.0 {
        r.push(format!("{:.0}% of early buyers are serial wallets", sf * 100.0));
    }
    r
}

/// CLI entry: screen the read-only snapshot and print JSON to stdout.
pub fn run(config: &Config, p: ScreenParams) -> Result<()> {
    let conn = db::open_reader(&config.snapshot_path)?;
    let out = screen(&conn, &p)?;
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
