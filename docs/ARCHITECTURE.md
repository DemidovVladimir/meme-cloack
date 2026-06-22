# Architecture

One Rust binary (`meme-expert`) with subcommands, an embedded DuckDB, and an
MCP server. The model/"intelligence" is not in Rust — it is the `SKILL.md` the
daily Claude run maintains.

## Modules (`src/`)

| File | Responsibility |
| --- | --- |
| `main.rs` | Load `.env`, install the rustls crypto provider, init tracing→stderr, dispatch the subcommand. |
| `cli.rs` | clap subcommands: `ingest`, `mcp`, `prune`, `stats`. |
| `config.rs` | `Config::from_env()` — all tunables + the survivor/retention policy. |
| `model.rs` | PumpPortal frame parsing → `NewToken` / `Trade` / `Control`. |
| `db.rs` | Schema, writer/reader connections, inserts, prune, snapshot rebuild, JSON queries. |
| `writer.rs` | Dedicated thread owning the DuckDB `Connection` (it's `!Sync`); drains a channel. |
| `pumpportal.rs` | Reconnecting websocket client; subscriptions are the source of truth, replayed on reconnect. |
| `ingest.rs` | Coordinator + the **40-minute survivor state machine**. |
| `mcp.rs` | rmcp stdio server: 7 read-only tools over the snapshot. |
| `prune.rs` / `stats.rs` | Manual maintenance / ops check. |

## Data flow

```text
[ws reader] --Frame--> mpsc --> [ingest coordinator] --WriteMsg--> [writer thread] --> hot.duckdb
                                       │                                                   │ every few min
                                       │ subscribe/unsubscribe                             ▼
                                  [pumpportal Controller] <─── ws sink              snapshot.duckdb (read-only)
                                                                                           │
                                                                          [mcp server] / [stats] read this
```

- DuckDB takes an **exclusive file lock** when held read-write, so no other process
  may open `hot.duckdb`. The ingester therefore rewrites a separate
  `snapshot.duckdb` (atomically) every few minutes; the MCP server and `stats`
  read that. This is why MCP works while the ingester runs.
- The writer thread is the single owner of the write connection. The Appender is
  created per-batch (never held across a prune), and new-token rows insert with
  `ON CONFLICT DO NOTHING` (PK-safe against reconnect replay).
- All logging goes to **stderr** because the `mcp` subcommand's **stdout is the
  JSON-RPC channel**.

## The 40-minute survivor policy (`ingest.rs`)

Per-token state (`created_ms`, running `peak_mcap`, `last_mcap`, `last_trade_ms`,
`subscribed`, `survived`). On launch: record the token and subscribe to its trades
(up to `MAX_ACTIVE_TRADE_SUBS`). Every sweep:

- **Before `SURVIVOR_AGE_MINUTES`:** drop the token (unsubscribe, evict) if it
  *collapsed* (`last_mcap < DEATH_DRAWDOWN_PCT × peak_mcap`) or went *silent*
  (no trades for `DEATH_SILENCE_MINUTES`). Its already-captured trades stay as
  negative examples until they age out.
- **At `SURVIVOR_AGE_MINUTES`:** mark it a survivor and keep tracking it.
- **At `RETENTION_HOURS`:** unsubscribe and evict; the writer prunes its rows.

Net: PumpPortal trade-stream spend scales with *survivors + tokens-in-their-first-
minutes*, not total launch volume, and the surfaced set is pre-filtered to
survivors.

## Reliability

- Reconnecting websocket with capped exponential backoff (500ms→60s, reset after
  30s healthy); subscriptions replayed from `SubState` on reconnect, with stale
  queued frames drained first to avoid double-billing.
- rustls crypto provider installed explicitly at startup (multiple providers are
  compiled; auto-selection would panic).
- Graceful shutdown on SIGTERM/Ctrl-C: flush buffer, final snapshot, stop writer.

## Schema

`new_tokens(mint PK, name, symbol, creator, created_ms, pool, market_cap_sol,
v_sol_in_curve, v_tokens_in_curve, initial_buy_sol, uri, signature, raw_json)`

`trades(mint, side, trader, signature, sol_amount, token_amount, market_cap_sol,
v_sol_in_curve, v_tokens_in_curve, pool, new_token_balance, ts_ms, raw_json)`

Time is epoch-millis (BIGINT). `raw_json` keeps the full frame so the daily
research can mine fields not promoted to columns. Indexes: `trades(ts_ms)`,
`trades(mint, ts_ms)`, `new_tokens(created_ms)`.
