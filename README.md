# meme-expert

A lean Rust system for spotting promising new Solana memecoins. It captures every
transaction of newly-created tokens into a rolling **24-hour DuckDB**, exposes that
data to Claude over **MCP**, and lets a **daily autonomous Claude run** discover
patterns on its own and write them into a `meme-expert` skill (`SKILL.md`). You
then ask the meme-expert agent to find live tokens matching those patterns.

The intelligence lives in `SKILL.md`, written by the AI — not in hand-coded
scoring. Rust just does reliable capture, 24h retention, and a query surface.

```text
PumpPortal websocket ── ingester (Rust) ── DuckDB (rolling 24h)
                              │                    │
                 40-min survivor state machine     │ snapshot every few min
                 (drop rugs <40m, keep survivors)  ▼
                                          MCP server (Rust, read-only)
                                                   │  stdio (over SSH from your Mac)
                                                   ▼
                       daily Claude run ── discovers patterns ── updates SKILL.md ── commits
                                                   │
                                          meme-expert agent ── finds live candidates
```

## Why this shape

- **Bots rotate strategies in days**, so patterns are rediscovered **daily** from
  fresh data rather than hand-maintained.
- **Most launches die in minutes.** The ingester drops any token that rugs or goes
  silent before **40 minutes**, so PumpPortal spend is bounded by *survivor* count,
  not *launch* count — and you only ever see survivors.
- **Raw data is disposable** (24h rolling); the distilled knowledge (`SKILL.md`) is
  what accumulates, in git.

## Build

Rust stable (1.91+). First build compiles the bundled DuckDB engine (~minutes).

```bash
cargo build --release      # binary at target/release/meme-expert
```

Ubuntu build deps: `sudo apt-get install -y build-essential cmake`.

## Run locally

```bash
cp .env.example .env        # set PUMPPORTAL_API_KEY (funded wallet) for trades
meme-expert ingest          # capture launches + survivor trades into ./data/hot.duckdb
meme-expert stats           # token/trade counts, top tokens (reads the snapshot)
```

The ingester writes a read-only `snapshot.duckdb` every few minutes; `stats` and
the MCP server read it, so they work while the ingester is running.

## MCP + the daily learning loop

The ingester runs on a server (see `docs/DEPLOY.md`); the **daily Claude research
runs on your Mac** on your subscription and reads the remote DB over **MCP-over-SSH**:

```bash
cp .mcp.json.example .mcp.json    # set your server host
deploy/run-daily-research.sh      # discovers patterns -> updates SKILL.md -> commits
```

Schedule it with `deploy/com.meme-expert.daily.plist` (launchd). Then, interactively:

```bash
claude            # from this repo, with .mcp.json present
> "Use the meme-expert skill and tools to screen tokens created in the last 30 minutes."
```

MCP tools (read-only over `new_tokens`, `trades`): `window_stats`,
`query_recent_tokens`, `top_tokens`, `token_detail`, `token_trades`,
`wallet_activity`, `run_readonly_sql`.

## CLI

| Command | What |
| --- | --- |
| `meme-expert ingest` | Run the PumpPortal ingester (24h rolling capture). |
| `meme-expert mcp` | MCP stdio server over the read-only snapshot. |
| `meme-expert stats` | Print DB stats / top tokens. |
| `meme-expert prune` | Manual retention sweep + snapshot rebuild (ingester must be stopped). |

See `docs/ARCHITECTURE.md` for internals and `docs/DEPLOY.md` for the Hetzner +
Mac setup. Candidate signals are research, not financial advice.
