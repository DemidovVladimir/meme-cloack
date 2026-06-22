//! DuckDB persistence: schema, the writer connection, read-only snapshot access,
//! inserts, retention prune, and the snapshot rebuild.
//!
//! Concurrency model: the ingester holds `hot.duckdb` read-write (DuckDB takes an
//! exclusive file lock). No other process may open that file. The MCP server and
//! `stats` therefore read a separate `snapshot.duckdb`, which the ingester
//! rewrites from its own connection every snapshot interval.

use anyhow::{Context, Result};
use duckdb::types::ValueRef;
use duckdb::{params, AccessMode, Config, Connection, DuckdbConnectionManager};
use serde_json::{Map, Value};
use std::path::Path;

use crate::model::{NewToken, Trade};

pub type ReaderPool = r2d2::Pool<DuckdbConnectionManager>;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS new_tokens (
    mint              VARCHAR PRIMARY KEY,
    name              VARCHAR,
    symbol            VARCHAR,
    creator           VARCHAR,
    created_ms        BIGINT  NOT NULL,
    pool              VARCHAR,
    market_cap_sol    DOUBLE,
    v_sol_in_curve    DOUBLE,
    v_tokens_in_curve DOUBLE,
    initial_buy_sol   DOUBLE,
    uri               VARCHAR,
    signature         VARCHAR,
    raw_json          VARCHAR NOT NULL
);
CREATE TABLE IF NOT EXISTS trades (
    mint              VARCHAR NOT NULL,
    side              VARCHAR NOT NULL,
    trader            VARCHAR NOT NULL,
    signature         VARCHAR,
    sol_amount        DOUBLE,
    token_amount      DOUBLE,
    market_cap_sol    DOUBLE,
    v_sol_in_curve    DOUBLE,
    v_tokens_in_curve DOUBLE,
    pool              VARCHAR,
    new_token_balance DOUBLE,
    ts_ms             BIGINT  NOT NULL,
    raw_json          VARCHAR NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_trades_ts      ON trades(ts_ms);
CREATE INDEX IF NOT EXISTS idx_trades_mint_ts ON trades(mint, ts_ms);
CREATE INDEX IF NOT EXISTS idx_tokens_created ON new_tokens(created_ms);
"#;

/// Open the writable hot database (creates parent dir + schema).
pub fn open_writer(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let conn = Connection::open(path).with_context(|| format!("open writer db {}", path.display()))?;
    conn.execute_batch(SCHEMA).context("init schema")?;
    Ok(conn)
}

/// Open a read-only connection (used by stats; MCP uses a pool).
pub fn open_reader(path: &Path) -> Result<Connection> {
    let config = Config::default().access_mode(AccessMode::ReadOnly)?;
    Connection::open_with_flags(path, config)
        .with_context(|| format!("open reader db {}", path.display()))
}

/// Build a small read-only connection pool for the MCP server.
pub fn reader_pool(path: &Path, max_size: u32) -> Result<ReaderPool> {
    let config = Config::default().access_mode(AccessMode::ReadOnly)?;
    let manager = DuckdbConnectionManager::file_with_flags(path, config)?;
    let pool = r2d2::Pool::builder().max_size(max_size).build(manager)?;
    Ok(pool)
}

/// Insert a new-token row. PK-safe: duplicates (reconnect replay) are ignored.
pub fn insert_new_token(conn: &Connection, t: &NewToken) -> Result<()> {
    conn.execute(
        "INSERT INTO new_tokens
         (mint, name, symbol, creator, created_ms, pool, market_cap_sol,
          v_sol_in_curve, v_tokens_in_curve, initial_buy_sol, uri, signature, raw_json)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)
         ON CONFLICT (mint) DO NOTHING",
        params![
            t.mint, t.name, t.symbol, t.creator, t.created_ms, t.pool, t.market_cap_sol,
            t.v_sol_in_curve, t.v_tokens_in_curve, t.initial_buy_sol, t.uri, t.signature, t.raw_json
        ],
    )?;
    Ok(())
}

/// Append a batch of trades via the fast Appender. The appender is created,
/// filled, flushed, and dropped here so it never overlaps a prune/DELETE.
pub fn append_trades(conn: &Connection, trades: &[Trade]) -> Result<()> {
    if trades.is_empty() {
        return Ok(());
    }
    let mut appender = conn.appender("trades")?;
    for t in trades {
        appender.append_row(params![
            t.mint, t.side, t.trader, t.signature, t.sol_amount, t.token_amount,
            t.market_cap_sol, t.v_sol_in_curve, t.v_tokens_in_curve, t.pool,
            t.new_token_balance, t.ts_ms, t.raw_json
        ])?;
    }
    appender.flush()?;
    Ok(())
}

/// Delete rows older than `cutoff_ms` and checkpoint.
pub fn prune(conn: &Connection, cutoff_ms: i64) -> Result<u64> {
    let a = conn.execute("DELETE FROM trades WHERE ts_ms < ?", params![cutoff_ms])?;
    let b = conn.execute("DELETE FROM new_tokens WHERE created_ms < ?", params![cutoff_ms])?;
    conn.execute_batch("CHECKPOINT")?;
    Ok((a + b) as u64)
}

/// Rebuild the read-only snapshot file from the live tables, atomically.
pub fn write_snapshot(conn: &Connection, snapshot_path: &Path) -> Result<()> {
    if let Some(parent) = snapshot_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = snapshot_path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(format!("{}.wal", tmp.display()));
    // Escape single quotes for the SQL string literal (a path may contain one,
    // e.g. /Volumes/John's Drive); otherwise ATTACH silently fails and snapshots
    // stop updating, leaving MCP/stats readers stale.
    let p = tmp.display().to_string().replace('\'', "''");
    conn.execute_batch(&format!(
        "ATTACH '{p}' AS snap;
         CREATE OR REPLACE TABLE snap.new_tokens AS SELECT * FROM new_tokens;
         CREATE OR REPLACE TABLE snap.trades AS SELECT * FROM trades;
         DETACH snap;"
    ))?;
    std::fs::rename(&tmp, snapshot_path)
        .with_context(|| format!("rename snapshot into {}", snapshot_path.display()))?;
    Ok(())
}

/// Run a query and return its rows as a JSON array of objects. Backbone of the
/// MCP tools.
pub fn query_json<P: duckdb::Params>(conn: &Connection, sql: &str, params: P) -> Result<Value> {
    let mut stmt = conn.prepare(sql)?;
    // Execute first: DuckDB only populates the result column schema after the
    // query runs, so column names are read from the running statement.
    let mut rows = stmt.query(params)?;
    let column_names: Vec<String> = rows
        .as_ref()
        .map(|s| s.column_names().into_iter().map(|c| c.to_string()).collect())
        .unwrap_or_default();
    let mut out: Vec<Value> = Vec::new();
    while let Some(row) = rows.next()? {
        let mut obj = Map::new();
        for (i, name) in column_names.iter().enumerate() {
            obj.insert(name.clone(), value_ref_to_json(row.get_ref(i)?));
        }
        out.push(Value::Object(obj));
    }
    Ok(Value::Array(out))
}

fn value_ref_to_json(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Boolean(b) => Value::Bool(b),
        ValueRef::TinyInt(n) => Value::from(n),
        ValueRef::SmallInt(n) => Value::from(n),
        ValueRef::Int(n) => Value::from(n),
        ValueRef::BigInt(n) => Value::from(n),
        ValueRef::HugeInt(n) => Value::from(n as i64),
        ValueRef::UTinyInt(n) => Value::from(n),
        ValueRef::USmallInt(n) => Value::from(n),
        ValueRef::UInt(n) => Value::from(n),
        ValueRef::UBigInt(n) => Value::from(n),
        ValueRef::Float(n) => json_num_f64(n as f64),
        ValueRef::Double(n) => json_num_f64(n),
        ValueRef::Text(bytes) => Value::String(String::from_utf8_lossy(bytes).into_owned()),
        other => Value::String(format!("{other:?}")),
    }
}

fn json_num_f64(n: f64) -> Value {
    serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null)
}
