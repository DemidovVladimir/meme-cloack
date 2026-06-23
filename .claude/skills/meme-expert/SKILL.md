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
`wallet_activity`, `window_stats`, `run_readonly_sql`, **`screen_candidates`**.

**`screen_candidates`** applies the Detection Heuristics below server-side and returns
a ranked candidate list — prefer it for live screening over re-deriving the SQL.
Args: `minutes` (default 20), `tier` (`balanced` default | `gate60` | `conviction60` |
`gate120` | `inflow120` | `sustained`), `limit` (default 30). Each row carries the
first-120s features (`buyers_60s/2m`, `buyers_min2`, `net_60s/2m`, `top_buy_share`,
`serial_frac`), a `sustained` flag, and `reasons[]`. It only scores tokens whose 120s
window has closed in the data (as-of = `max(ts_ms)`), so results are never half-baked.

Only tokens that survived ~40 minutes without rugging are tracked to maturity, so
the dataset is already filtered to candidates worth analyzing (plus the early
trades of tokens that died young, as negative examples).

## Two modes

- **Screen live tokens** (interactive): the authoritative path is **SKILL.md-driven** —
  call `screen_candidates(tier=all, minutes=40, limit=100)` to get the unfiltered
  first-120s feature rows for every recent token (created in the last ~40 min, 120s
  window closed), then **rank them by applying the Detection Heuristics in THIS file**
  (which the daily research keeps current) and report the top few with the matched
  signals + risks. For a quick deterministic shortlist that mirrors the committed
  heuristics, `screen_candidates(tier=balanced)` is the fast path. Fall back to
  `top_tokens`/`run_readonly_sql` for ad-hoc views. Always say this is research, not
  financial advice.
- **Daily research** (autonomous): see `deploy/research-prompt.md`. Discover new
  patterns, update the Detection Heuristics and Research Log here, commit.

## Outcome Definition

Launch market cap is tightly clustered at the pump.fun baseline **~28 SOL** (p10–p90
= 28–34), so outcomes are measured as a multiple of launch. Of the labelable cohort
(n=1318), the outcome tiers and their base rates:

- **WINNER / "pumped"** = peak `market_cap_sol` ≥ **3× launch** (~≥ 84 SOL).
  n=69, **base rate 5.2%**. The primary learnable target. Splits into:
  - **SUSTAINED** = winner that kept trading **≥ 10 min**. n=22 (**1.7%**). The
    closest thing to "exitable" — but even these retain only ~21% of peak by the end,
    so they are momentum trades, not holds.
  - **SPIKE** (pump-and-dump) = winner that died **< 10 min**. n=47. Often round-trips
    in 1–5 min; breadth predicts the spike but not your ability to exit.
- **RUG / DEAD** = stopped trading within ~2 min with peak < 1.5× launch. n=769,
  **~58%** — the modal outcome. Median token lifespan across the cohort was **1 min**.
- **TRUE SURVIVOR** (≥ 40 min continuous) is vanishingly rare (9/1318) — aspirational,
  not modeled; SUSTAINED (≥10 min) is the practical "real run" target.

Signals are evaluated at two live decision points — **60 s** and **120 s** after
`created_ms` — plus a launch-time (t0) pre-filter. All are computable from `trades`
in real time and predict the *eventual* tier.

## Detection Heuristics

The dominant axis separating winners from rugs is **breadth of genuine early demand**:
how many *distinct* wallets buy, whether net SOL actually flows *in*, and whether that
buying *continues into the 2nd minute* — not trade count (easy to wash) and not the
creator's behavior. The single best "exitable run" signal is a **large net inflow with
buying that keeps pulling in fresh wallets past the first 60 s**.

Live feature dictionary (per candidate `mint`):
- `dev_buy`      = `new_tokens.initial_buy_sol`  (creator's launch buy; known at t0)
- `buyers_60s`   = `COUNT(DISTINCT trader)` for `ts_ms <= created_ms+60000`
- `net_60s`      = Σ buy SOL − Σ sell SOL within 60 s
- `buyers_2m`    = `COUNT(DISTINCT trader)` for `ts_ms <= created_ms+120000`
- `buyers_min2`  = `COUNT(DISTINCT trader)` for `60000 < ts−created <= 120000`  (2nd-minute breadth)
- `net_2m`       = Σ buy SOL − Σ sell SOL within 120 s
- `top_buy_share`= `MAX(buy SOL per trader) / SUM(buy SOL)` within 120 s  (whale capture)
- `serial_frac`  = (early buyers that are *serial* wallets) / (early buyers), where a
  serial wallet is one seen buying **≥ 20 distinct tokens** in the rolling window
  (needs a maintained rolling serial-wallet set to compute live)

**Decision flow:** t0 dev-buy pre-filter → 60 s gate (H1/H2) → 120 s tiers (H3–H6),
minus the avoid filters (H7/H8). Numbers are vs the 5.2% winner base unless noted.

HEURISTIC: Dev-buy size (t0 leading pre-filter)
SIGNAL: `dev_buy < 0.5` skews rug; `dev_buy >= 1.5` skews survivor.
EVIDENCE: median `dev_buy` rises monotonically rug 0.49 → mid 1.48 → spike 1.48 → sustain 2.24 SOL. Heavy overlap — weak alone, but it's the only signal available *before any trade*.
CONFIDENCE: low (as of 2026-06-23).
ACTION: tiny dev buy = mild caution; never act on this alone.

HEURISTIC: 60-second breadth gate (earliest filter)
SIGNAL: `buyers_60s >= 8`.
EVIDENCE: flags 363, precision 17%, **recall 90%** (~3.3×). Catches almost every winner one minute in.
CONFIDENCE: medium (as of 2026-06-23).
ACTION: `< 8` buyers by 60 s → ignore. `>= 8` → watch; check H2.

HEURISTIC: 60-second conviction (act early)
SIGNAL: `buyers_60s >= 12 AND net_60s > 0`.
EVIDENCE: flags 113, **precision 43%**, recall 71% (~8.3×) — at just 60 s, as good as the 2-min rule.
CONFIDENCE: medium (as of 2026-06-23).
ACTION: strong enough to act on at the 1-minute mark.

HEURISTIC: 120-second breadth gate
SIGNAL: `buyers_2m >= 10`.
EVIDENCE: flags 304, precision 21%, **recall 93%** (~4×). Winner median `buyers_2m`=99 vs 3 for rugs.
CONFIDENCE: medium (as of 2026-06-23).
ACTION: hard gate — below this, do not surface.

HEURISTIC: Breadth + net inflow
SIGNAL: `buyers_2m >= 10 AND net_2m > 0`.
EVIDENCE: flags 111, **precision 40%**, recall 64% (~7.5×). Winners net +9.2 SOL median; rugs/mids run net-*negative* (creator/snipers distributing).
CONFIDENCE: medium (as of 2026-06-23).

HEURISTIC: High-conviction — predicts a ≥3× PEAK
SIGNAL: `buyers_2m >= 20 AND net_2m >= 2 AND top_buy_share < 0.5`.
EVIDENCE: flags 65, **precision 63%**, recall 59% (~12×). Adding `serial_frac < 0.4` → 64.5% (only +3 dropped; redundant with breadth).
CONFIDENCE: low–medium (as of 2026-06-23) — thresholds from one window.
ACTION: surface near the top; expect a spike, not necessarily a hold.

HEURISTIC: Sustained tier — predicts a ≥3× peak that LASTS ≥10 min (most "exitable")
SIGNAL: `net_2m >= 10 AND buyers_min2 >= 30`  (big inflow AND buying continues into minute 2). Looser: `net_2m >= 5 AND buyers_min2 >= 20` → 45% prec / **82% recall**.
EVIDENCE: tight rule flags 27, **precision 48%**, recall 59% (~29× over the 1.7% sustained base). Sustained vs spike: net +15.8 vs +2.7 SOL; 2nd-min buyers 74 vs 31; momentum (min2/min1 trades) 0.65 vs 0.38.
CONFIDENCE: low–medium (as of 2026-06-23) — only 22 positives.
ACTION: top tier. Demand that keeps compounding past 60 s = a real run forming.

HEURISTIC: Avoid — thin and bleeding (kill-filter)
SIGNAL: `net_2m < 0 AND buyers_2m < 5`.
EVIDENCE: 513 matched → **88.7% rugged, 0 of 513 ever won.**
CONFIDENCE: medium (as of 2026-06-23).
ACTION: hard-skip; never surface.

HEURISTIC: Avoid — sniper-dominated (no organic crowd)
SIGNAL: `serial_frac > 0.5` (early buyers are mostly serial bot/sniper wallets), especially with low breadth.
EVIDENCE: median `serial_frac` rug 0.83 → mid 0.50 → spike 0.18 → sustain 0.17. 257 wallets (≥20 tokens each, max 851) generate 28% of *all* trades; they snipe everything, so a token bought *only* by them lacks real demand.
CONFIDENCE: low–medium (as of 2026-06-23); correlated with low breadth, and needs a live serial-wallet set.
ACTION: down-rank/skip when serial fraction is high and organic (one-off) buyers are absent.

HEURISTIC: Creator early-sell is near-universal — weak on its own
SIGNAL: token `creator` appears as `side='sell'` within 120 s.
EVIDENCE: 998/1318 (76%) — dev dumping is the norm. Rug 65.5% vs 58.3% base; win 4.2% vs 5.2%. Barely discriminative as a binary.
CONFIDENCE: low (as of 2026-06-23).
ACTION: do NOT use as a standalone filter; at most a mild tiebreaker. (TODO: test creator-sell *magnitude* / fraction of supply.)

<!-- Each heuristic uses this format:
HEURISTIC: <short name>
SIGNAL: <precise condition on captured fields + time window, computable live>
EVIDENCE: <sample size, separation / precision / lift observed>
CONFIDENCE: <low|medium|high> (as of <YYYY-MM-DD>)
ACTION: <what a live screen should do when it matches>
-->

## Research Log

- **2026-06-23 — second pass** (same frozen window). Added tiers/features:
  - **Sustained vs spike:** split winners into SUSTAINED (≥10 min, n=22) vs SPIKE
    (n=47). Separator = **net inflow magnitude + 2nd-minute buyer breadth** (sustain
    net +15.8 vs +2.7; min-2 buyers 74 vs 31; momentum 0.65 vs 0.38). New sustained
    rule: `net_2m≥10 AND buyers_min2≥30` → 48% prec / 29× lift. Both tiers retain only
    ~21% of peak by end → momentum trades, not holds.
  - **60 s gate:** `buyers_60s≥12 AND net_60s>0` → 43% prec / 71% recall — acts a full
    minute earlier with no precision loss.
  - **Serial-sniper clustering:** 257 wallets (≥20 tokens, max 851) make 28% of all
    trades. `serial_frac` (share of early buyers that are serial) is **inverse** to
    success: rug 0.83 → sustain 0.17. Strong as an *avoid* signal; only marginal
    (+1.4 pp) as a chase-rule booster (redundant with breadth).
  - **Dev-buy (t0):** median `initial_buy_sol` monotonic rug 0.49 → sustain 2.24 —
    the only pre-trade signal; weak but leading.
- **2026-06-23 — first research run** (frozen snapshot, not live; ingestion was
  stopped 2026-06-23 after the PumpPortal key drained — see meme-ops / memory).
  - **Dataset caveat:** trades cover a **single ~100-min window** (2026-06-22
    20:38–22:18 UTC): 13,048 tokens discovered but only **2,312 ever got trade
    data** (sub cap `MAX_ACTIVE_TRADE_SUBS=500`). Labelable cohort = **1,318**
    tokens born in the first 50 min (enough observation room); **69 winners** (≥3×).
    Treat all confidences as provisional until revalidated over more/independent
    windows.
  - **Method:** per-token early features over first 120 s vs eventual peak/lifespan.
  - **Findings:** distinct early-buyer breadth + net SOL inflow + low whale-capture
    cleanly separate winners from rugs (12× lift at the high-conviction tier).
    Net-outflow + thin-buyers is a 0-winner kill-filter. Trade *count* and creator
    early-sell are weak/non-discriminative. Median token lives **1 minute**.
  - **Limitations:** "lifespan" is bounded by the ingester's tracking policy (sub cap
    + 40-min dead-sweep), so it's a proxy, not true on-chain life. Mild leakage where
    a winner peaks inside the 2-min feature window (acceptable: features are what
    you'd actually have at the 2-min decision point).
  - **Next run TODO:** creator-sell magnitude; 30/60 s windows for earlier entry;
    trader-clustering / shared-funder detection; revalidate thresholds on fresh data.
- (initial) Skill created; awaiting first autonomous run.
