# meme-expert daily autonomous research

You have READ-ONLY access to the last ~24h of Solana memecoin data through the
`meme-expert` MCP server. Tools: `window_stats`, `query_recent_tokens`,
`top_tokens`, `token_detail`, `token_trades`, `wallet_activity`,
`run_readonly_sql`. Tables: `new_tokens` and `trades`. Time columns are BIGINT
epoch-millis (`created_ms`, `ts_ms`) — do all time math in millis, never SQL
`now()`/intervals.

The dataset is already filtered: only tokens that survived ~40 minutes without
rugging are tracked to maturity, plus the early trades of tokens that died young
(your negative examples).

## Mission

AUTONOMOUSLY and WITHOUT a fixed script, discover concrete, falsifiable patterns
that separate tokens that PUMPED from those that did NOT. You define "pumped"
from the data (e.g. sustained market_cap_sol growth vs round-tripping). Explore
freely — wallet behavior, trade timing/cadence, buyer clustering, creator/funding
patterns, buy/sell imbalance, holder concentration, anything the data supports.
Form hypotheses, test them with `run_readonly_sql`, keep only what holds up.

## Method

1. Call `window_stats` first. If `trades` is 0, STOP and append a Research Log note
   that the trade stream was unavailable (likely missing API key / wallet balance) —
   do not invent patterns.
2. Establish the outcome variable (pumped vs not) from market_cap_sol trajectories.
3. Iterate: hypothesize a signal → query → measure how well it separates the two
   groups (sample size, precision, lift). Discard weak signals.
4. Prefer signals computable in a token's FIRST FEW MINUTES (actionable for live
   detection), using only fields the live ingester captures.

## Deliverable — update the skill, do not overwrite knowledge

Edit `.claude/skills/meme-expert/SKILL.md`:
- ADD newly-supported heuristics under "## Detection Heuristics" in the file's format.
- REVISE confidence on existing heuristics if today's data confirms/contradicts them
  (note the date). Remove a heuristic ONLY if the data now clearly refutes it.
- Refine the "## Outcome Definition" if warranted.
- Append a dated one-paragraph entry under "## Research Log" summarizing what you found.

Keep SKILL.md concrete and applicable — it is durable knowledge; the raw data is disposable.

## Finish

- `git add .claude/skills/meme-expert/SKILL.md`
- `git commit -m "patterns: daily meme-expert research"`
- Do NOT push, rebase, reset, or touch anything outside the skill directory.
