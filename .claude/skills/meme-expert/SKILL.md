---
name: meme-expert
description: Detect promising new Solana memecoins. Use when asked to assess, screen, or find live tokens worth watching, or to analyze a token/wallet's behavior. Holds detection heuristics learned daily from the rolling 24h PumpPortal dataset (wallet behavior, trade timing, clustering, funding) and the MCP tools to apply them.
allowed-tools: Read, Edit, mcp__meme-expert, Bash(git add:*), Bash(git commit:*), Bash(git status:*), Bash(git diff:*)
---

# Meme-Expert

Durable, accumulating knowledge for spotting Solana memecoins likely to pump,
plus how to query the live data. This file is rewritten **daily** by an
autonomous research run over the last 24h of data — treat the heuristics below as
the current best model, not gospel, and revise them when the data disagrees.

## Data access (MCP server `meme-expert`)

The `meme-expert` MCP server is read-only over the last ~24h of launches/trades.
Tables: `new_tokens`, `trades`. **Time columns are epoch-millis** (`created_ms`,
`ts_ms`) — do all time math in millis, never SQL `now()`/intervals.

Tools: `query_recent_tokens`, `token_detail`, `token_trades`, `top_tokens`,
`wallet_activity`, `window_stats`, `run_readonly_sql`.

Only tokens that survived ~40 minutes without rugging are tracked to maturity, so
the dataset is already filtered to candidates worth analyzing (plus the early
trades of tokens that died young, as negative examples).

## Two modes

- **Screen live tokens** (interactive): pull recent/top tokens, score each against
  the Detection Heuristics below, report a short ranked list with the matched
  signals and the risks. Always say this is research, not financial advice.
- **Daily research** (autonomous): see `deploy/research-prompt.md`. Discover new
  patterns, update the Detection Heuristics and Research Log here, commit.

## Outcome Definition

<working definition of "pumped" — refined by the daily research from market_cap_sol
trajectories; e.g. sustained N× growth after the first minutes without round-tripping.>

## Detection Heuristics

<none yet — populated by the first daily research run.>

<!-- Each heuristic uses this format:
HEURISTIC: <short name>
SIGNAL: <precise condition on captured fields + time window, computable live>
EVIDENCE: <sample size, separation / precision / lift observed>
CONFIDENCE: <low|medium|high> (as of <YYYY-MM-DD>)
ACTION: <what a live screen should do when it matches>
-->

## Research Log

- (initial) Skill created; awaiting first autonomous run.
