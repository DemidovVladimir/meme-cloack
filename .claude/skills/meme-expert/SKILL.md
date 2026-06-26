---
name: meme-expert
description: Detect promising new Solana memecoins. Use when asked to assess, screen, or find live tokens worth watching, or to analyze a token/wallet's behavior. Holds detection heuristics learned from the rolling 24h Helius (pump.fun firehose) dataset — wallet behavior, trade timing, breadth, clustering, funding — plus the MCP tools to apply them.
allowed-tools: Read, Edit, mcp__meme-expert, Bash(git add:*), Bash(git commit:*), Bash(git status:*), Bash(git diff:*)
---

# Meme-Expert

Durable, accumulating knowledge for spotting Solana memecoins likely to pump,
plus how to query the live data. This file is rewritten by research runs over the
rolling 24h of data — treat the heuristics below as the current best model, not
gospel, and revise them when the data disagrees.

## Data access (MCP server `meme-expert`)

Read-only over the last ~24h of launches/trades — now the **complete pump.fun
firehose** (Helius LaserStream WebSocket): every token creation and every trade,
rugs included, no sub cap. Tables: `new_tokens`, `trades`. **Time columns are
epoch-millis** (`created_ms`, `ts_ms`) — do all time math in millis, never SQL
`now()`/intervals.

Tools: `query_recent_tokens`, `token_detail`, `token_trades`, `top_tokens`,
`wallet_activity`, `window_stats`, `run_readonly_sql`, **`screen_candidates`**.

**`screen_candidates`** applies the Detection Heuristics below server-side and returns
a ranked candidate list — prefer it for live screening over re-deriving the SQL.
Args: `minutes` (default 20), `tier` (`balanced` default | `gate60` | `conviction60` |
`gate120` | `inflow120` | `sustained` | `all`), `limit` (default 30). Each row carries the
first-120s features (`buyers_60s/2m`, `buyers_min2`, `net_60s/2m`, `top_buy_share`,
`serial_frac`), a `sustained` flag, and `reasons[]`. It only scores tokens whose 120s
window has closed in the data (as-of = `max(ts_ms)`), so results are never half-baked.

## Outcome Definition

Launch market cap is tightly clustered at the pump.fun baseline **~28 SOL** (p10–p90
≈ 28–34). The dominant realized outcome is **bonding-curve completion ("graduation"
to Raydium)**: a graduating token's `market_cap_sol` peaks hard at the curve-completion
cap **~411 SOL (~14× launch)** — peaks pile up almost exactly there. So in this data:

- **WINNER / "pumped" = peak `market_cap_sol` ≥ 3× launch (≈ ≥ 84 SOL)** ≈ **graduated**.
  On the full 24h firehose (n = **27,863** launches with a closed 120s window):
  - **≥3× (graduated): 3,068 → base rate 11.0%.**  ≥5× (≥140 SOL): ~5.5%.  ≥10× (≥280 SOL): **579 → 2.08%.**
- **DID NOT GRADUATE / rug**: ~89%. Most die on the curve within ~1–2 min.

**IMPORTANT measurement caveat — the graduation ceiling.** The firehose only sees the
pump.fun program, so a token's observable `market_cap_sol` **caps at graduation (~411
SOL)**; everything that happens after it migrates to Raydium is invisible. Therefore
"winner" here means **"filled the bonding curve,"** NOT "would have made you money to
hold." True exitability / post-grad performance is currently UNMEASURABLE — see the
Research Log TODO (add Raydium/AMM tracking). Treat graduation as the learnable target
and remember a graduate can still dump immediately on Raydium.

> Base-rate revision (2026-06-25): the old 5.2% "winner" rate came from the
> PumpPortal era when only ≤500 tokens had trade data (sub cap) — biased and
> incomplete. The complete firehose puts the true ≥3×/graduation rate at **11.0%**.
> The previous SPIKE-vs-SUSTAINED (≥10 min) split was lifespan-based on that capped
> data; it is not re-derived here because lifespan past graduation is unobservable.

Signals are evaluated at two live decision points — **60 s** and **120 s** after
`created_ms` — plus a launch-time (t0) pre-filter. All are computable from `trades`
in real time and predict the *eventual* tier. NOTE a mild leakage: the fastest
graduates fill the curve **inside** the 120 s feature window (net_2m → ~85 SOL, the
full curve, with `buyers_min2`=0), so for them the 2-min signal is partly *detection*
of an in-progress graduation rather than pure prediction.

## Detection Heuristics

The dominant axis separating winners from rugs is **breadth of genuine early demand**:
how many *distinct* wallets buy, whether net SOL actually flows *in*, and whether that
buying *continues into the 2nd minute* — not trade count (easy to wash) and not the
creator's behavior. The single best signal is a **large net inflow with buying that
keeps pulling in fresh wallets past the first 60 s**.

Live feature dictionary (per candidate `mint`):
- `dev_buy`      = `new_tokens.initial_buy_sol`  (creator's launch buy; known at t0)
- `buyers_60s`   = `COUNT(DISTINCT trader)` for `ts_ms <= created_ms+60000`
- `net_60s`      = Σ buy SOL − Σ sell SOL within 60 s
- `buyers_2m`    = `COUNT(DISTINCT trader)` for `ts_ms <= created_ms+120000`
- `buyers_min2`  = `COUNT(DISTINCT trader)` for `60000 < ts−created <= 120000`  (2nd-minute breadth)
- `net_2m`       = Σ buy SOL − Σ sell SOL within 120 s (→ ~85 SOL = curve nearly full = imminent graduation)
- `top_buy_share`= `MAX(buy SOL per trader) / SUM(buy SOL)` within 120 s  (whale capture)
- `serial_frac`  = (early buyers that are *serial* wallets) / (early buyers); serial = wallet seen buying **≥ 20 distinct tokens** in the window

**Decision flow:** t0 dev-buy pre-filter → 60 s gate (H1/H2) → 120 s tiers (H3–H6),
minus the avoid filters (H7/H8). Precision/recall/lift below are vs the **11.0%**
graduation base on the 2026-06-25 full-firehose 24h (n=27,863) unless noted.

HEURISTIC: Dev-buy size (t0 leading pre-filter)
SIGNAL: `dev_buy < 0.5` skews rug; `dev_buy >= 1.5` skews graduate.
EVIDENCE: median `dev_buy` rises monotonically rug → graduate (frozen window). Heavy overlap — weak alone, but the only signal available *before any trade*.
CONFIDENCE: low (as of 2026-06-23; not re-tested 2026-06-25).
ACTION: tiny dev buy = mild caution; never act on this alone.

HEURISTIC: 60-second breadth gate (earliest filter)
SIGNAL: `buyers_60s >= 8`.
EVIDENCE: full firehose 2026-06-25 — flags 6,181, **precision 30.7%, recall 61.9%** (2.8× lift). Catches most eventual graduates one minute in.
CONFIDENCE: medium-high (validated across two windows).
ACTION: `< 8` buyers by 60 s → ignore. `>= 8` → watch; check H2.

HEURISTIC: 60-second conviction (act early)
SIGNAL: `buyers_60s >= 12 AND net_60s > 0`.
EVIDENCE: 2026-06-25 — flags 3,987, **precision 36.5%, recall 47.5%** (3.3× lift) at just 60 s. Acts a full minute earlier than the 2-min rules with comparable precision.
CONFIDENCE: medium-high (as of 2026-06-25).
ACTION: strong enough to act on at the 1-minute mark.

HEURISTIC: 120-second breadth gate
SIGNAL: `buyers_2m >= 10`.
EVIDENCE: 2026-06-25 — flags 5,342, **precision 34.5%, recall 60.1%** (3.1× lift). Best recall/precision balance for a hard gate.
CONFIDENCE: high (validated across two windows).
ACTION: hard gate — below this, do not surface.

HEURISTIC: Breadth + net inflow
SIGNAL: `buyers_2m >= 10 AND net_2m > 0`.
EVIDENCE: 2026-06-25 — flags 4,874, **precision 36.1%, recall 57.4%** (3.3× lift). Net-positive inflow lifts precision over the bare gate at small recall cost.
CONFIDENCE: high (as of 2026-06-25).

HEURISTIC: High-conviction — predicts graduation (≥3× peak)
SIGNAL: `buyers_2m >= 20 AND net_2m >= 2 AND top_buy_share < 0.5`.
EVIDENCE: 2026-06-25 full firehose — flags 1,561, **precision 62.3%, recall 31.7%** (5.7× lift) for ≥3×; for **≥10×: precision 15.2%, recall 40.9%** (7.3× lift). Precision matches the frozen backtest's 63% almost exactly — **validated out-of-sample.** High precision, moderate recall (misses low-breadth/whale-driven graduates).
CONFIDENCE: medium-high (consistent across two independent windows).
ACTION: surface near the top; ~62% graduate, but the peak is the graduation cap — exit risk is real.

HEURISTIC: Sustained tier — strongest graduation predictor
SIGNAL: `net_2m >= 10 AND buyers_min2 >= 30` (big inflow AND breadth still growing in minute 2).
EVIDENCE: 2026-06-25 — flags 619, **precision 95.6%, recall 19.3%** for ≥3×/graduation (8.7× lift). Near-certain the curve fills once you see ≥10 SOL net + ≥30 fresh 2nd-minute buyers. BLIND SPOT: by requiring `buyers_min2 >= 30` it MISSES the fastest graduates, which fill the curve in <60 s and have `buyers_min2`=0 (catch those via High-conviction / net_2m→85).
CONFIDENCE: medium-high (as of 2026-06-25; precision is for graduation, not post-grad holding).
ACTION: top tier. Highest-precision "will graduate" signal available.

HEURISTIC: Avoid — thin and bleeding (kill-filter)
SIGNAL: `net_2m < 0 AND buyers_2m < 5`.
EVIDENCE: 2026-06-25 — 199 matched, **only 3 graduated (1.5%)** vs 11% base. Frozen window: 0 of 513. Consistently a near-zero-win population.
CONFIDENCE: high (validated across two windows).
ACTION: hard-skip; never surface.

HEURISTIC: Avoid — sniper-dominated, low breadth
SIGNAL: `serial_frac > 0.5 AND buyers_2m < 10` (early buyers mostly serial bots, no organic crowd).
EVIDENCE: 2026-06-25 — 14,317 matched, **7.8% graduated** (below the 11% base, ~0.7×). A *mild* negative, not a kill — and a broad net (it would discard 1,115 eventual graduates). Serial fraction is best as a down-ranker, redundant with breadth.
CONFIDENCE: medium (as of 2026-06-25).
ACTION: down-rank when serial fraction is high AND organic buyers are absent; do not hard-skip on its own.

HEURISTIC: Creator early-sell is near-universal — weak on its own
SIGNAL: token `creator` appears as `side='sell'` within 120 s.
EVIDENCE: 998/1318 (76%) on the frozen window — dev dumping is the norm; barely discriminative as a binary. Not re-tested 2026-06-25.
CONFIDENCE: low.
ACTION: do NOT use as a standalone filter; at most a mild tiebreaker. (TODO: test creator-sell *magnitude* / fraction of supply.)

<!-- Each heuristic uses this format:
HEURISTIC: <short name>
SIGNAL: <precise condition on captured fields + time window, computable live>
EVIDENCE: <sample size, separation / precision / lift observed>
CONFIDENCE: <low|medium|high> (as of <YYYY-MM-DD>)
ACTION: <what a live screen should do when it matches>
-->

## Trading Notes (paper → live)

Durable record of the trading experiment so we DON'T re-run the expensive paper trial
every time. Update this section when new paper/live results land; read it before any trade.

**Bottom line so far: the detection edge does NOT translate into trading profit.**
Predicting graduation (62% precision) ≠ a profitable buy. Honest prior: negative EV.

**Why the signal isn't tradeable** (2026-06-25 backtest, n=1,480 high-conviction):
- Buying the signal at the 2-min mark: median *best-possible* exit (sell the post-entry
  peak — unachievable) = **+16%**; **35%** go flat/down; only **15%** ever double → negative EV after costs.
- The signal is **coincident with the move**: by 60–120s the token already ran to a median
  ~54 SOL (~2× launch). You buy after the pump and become exit liquidity.
- Memecoins are **negative-sum / adversarial**: profit accrues to devs, sub-second snipers/bots,
  and insiders who *manufacture* the early breadth (that's the `serial_frac` cohort).

**Cost model** (use for every sim / live sizing): ~1% pump.fun fee + ~1.5% slippage **per side**
≈ 5% round-trip drag. Peak is censored at graduation (~411 SOL); post-Raydium price is invisible.

**Harness — `meme-expert papertrade`** (re-run to test/adjust; don't guess):
- Decoupled live screener + simulated P&L; its own Helius stream; no keys/execution.
  Logs fee+slippage-adjusted trades to `data/paper_trades.jsonl`. systemd: `meme-expert-papertrade`.
- Knobs (env `PAPER_*` / CLI): `--entry-secs`(60) `--min-buyers`(12) `PAPER_MIN_NET_SOL`(2)
  `PAPER_MAX_TOP_SHARE`(0.5) `--tp`(0.5) `--sl`(0.3) `--hold-secs`(300) `PAPER_GRAD_MCAP`(400)
  `PAPER_FEE_PCT`(0.01) `PAPER_SLIP_PCT`(0.015) `PAPER_POSITION_SOL`(0.1).
- **Gate:** only consider real funds if net P&L is *clearly & repeatably positive after costs.*

**Paper trial CONCLUDED 2026-06-26 — 24h, n=1,915, clean run (0 restarts). GATE NOT CLEARED.**
Net **−18.69 SOL** after costs (0.1 SOL/position), win rate **28.9%**, mean **−9.8%/trade**
(median −29% = a near-full stop). Exits: stop_loss 880/46% (−31.6 SOL), timeout 569/30%
(−7.5 SOL), take_profit 443/23% (+20.4 SOL), graduation 23 (+0.04) — winners can't cover the
stops. **Avg entry mcap 71.7 SOL (~2.6× the ~28 launch) — we buy AFTER the move, every time.**
Loss was **stable across all three 8h thirds** (−6.5 / −4.5 / −7.6 SOL) → not a one-window fluke.
**CONCLUSION: these heuristics are a graduation *detector*, not a profitable entry signal. Do NOT
trade real funds on them as-is.** The structural blockers (sub-second latency + Raydium post-grad
data) remain hard prerequisites. Service stopped 2026-06-26. To revisit, adjust the `papertrade`
knobs and re-run — but expect the same unless the latency/Raydium problems are solved first.
Ledger preserved at `data/paper_trades.jsonl`; re-summarize via meme-ops `paper-stats`.

**Two structural blockers to any real edge** (neither is a pattern tweak):
1. **Latency** — 5–15 min snapshot lag, and even a 60s decision is too slow vs first-second
   snipers. Needs sub-second live alerting/execution.
2. **Post-graduation blindness** — the big moves are on Raydium, which we don't ingest. Needs an AMM source.
Until BOTH are solved, **do not trade real funds.**

## Research Log

- **2026-06-25 — first full-firehose 24h run** (Helius, 2026-06-23 21:07 → 06-24 21:17
  UTC). Dataset: **27,965 tokens · 1,894,214 trades · 98,897 distinct traders** — every
  pump.fun trade, no 500-sub cap (vs the frozen 100-min/1,318-labelable set). Findings:
  - **Outcome = graduation.** Peaks cluster hard at the **~411 SOL curve-completion cap**;
    "≥3×" ≈ "graduated." True base rate is **11.0%** (3,068/27,863), not the old sub-cap
    5.2%. ≥10× = 2.08%. Peak is censored at graduation — post-Raydium life is invisible.
  - **High-conviction VALIDATED out-of-sample:** 62.3% precision (≈ the frozen 63%),
    31.7% recall, 5.7× lift; ≥10×: 15.2% prec / 7.3× lift. The breadth+inflow+anti-whale
    rule generalizes.
  - **Sustained = strongest graduation signal:** `net_2m≥10 AND buyers_min2≥30` →
    95.6% precision (619 flagged) — but structurally misses sub-60 s graduates.
  - **Precision/recall map** (vs 11% base): gate60 30.7%/61.9%; conviction60 36.5%/47.5%;
    gate120 34.5%/60.1%; inflow120 36.1%/57.4%. Use gate120 for recall, high-conviction
    for precision, sustained for near-certainty.
  - **Kill-filter holds:** thin+bleeding (net_2m<0 & buyers_2m<5) graduated 1.5% (3/199).
    Sniper+low-breadth only mildly negative (7.8%) — keep as down-ranker, not a hard skip.
  - **Best runs:** Pokelana (14.2×, 321 early buyers, +58.8 net, ~148 min), YSY, X-Ray,
    AC, STARMIND, WEN — all graduated.
  - **TODOs:** (1) add Raydium/post-graduation tracking to measure true exitability
    (current ceiling is graduation); (2) net_2m→85 as an explicit "imminent graduation"
    flag; (3) creator-sell *magnitude*; (4) revalidate thresholds on a 2nd independent 24h.
- **2026-06-23 — second pass** (frozen window). Added tiers/features:
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
    tokens; **69 winners** (≥3×). Superseded by the 2026-06-25 full-firehose run.
  - **Method:** per-token early features over first 120 s vs eventual peak/lifespan.
  - **Findings:** distinct early-buyer breadth + net SOL inflow + low whale-capture
    cleanly separate winners from rugs. Net-outflow + thin-buyers is a 0-winner
    kill-filter. Trade *count* and creator early-sell are weak/non-discriminative.
  - **Limitations:** "lifespan" was bounded by the ingester's sub cap + 40-min
    dead-sweep, so it was a proxy, not true on-chain life.
- (initial) Skill created; awaiting first autonomous run.
