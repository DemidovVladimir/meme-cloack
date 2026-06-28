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
to PumpSwap AMM)**: a graduating token's `market_cap_sol` peaks hard at the curve-completion
cap **~411 SOL (~14× launch)** — peaks pile up almost exactly there. So in this data:

- **WINNER / "pumped" = peak `market_cap_sol` ≥ 3× launch (≈ ≥ 84 SOL)** ≈ **graduated**.
  On the full 24h firehose (n = **27,863** launches with a closed 120s window):
  - **≥3× (graduated): 3,068 → base rate 11.0%.**  ≥5× (≥140 SOL): ~5.5%.  ≥10× (≥280 SOL): **579 → 2.08%.**
- **DID NOT GRADUATE / rug**: ~89%. Most die on the curve within ~1–2 min.

**IMPORTANT measurement caveat — the graduation ceiling.** The firehose only sees the
pump.fun **bonding-curve** program, so a token's observable `market_cap_sol` **caps at
graduation (~411 SOL)** and then **freezes**. In our data a graduated token shows up as
**`market_cap_sol`≈411 + no new trades (high `idle`/`last_trade_age`)** — that signature
means it **MIGRATED to PumpSwap AMM** (pump.fun's own AMM — **NOT Raydium**; current
pump.fun graduates to PumpSwap), **not that it died.** Therefore "winner" here means
**"filled the bonding curve,"** NOT "would have made you money to hold."
**Post-grad price is NOT unmeasurable — it's just not in the firehose.** It is queryable
**on-demand via Helius RPC**: read the PumpSwap pool reserves (`market_cap ≈ wsol_reserve /
token_reserve × 1e9`) or parse recent swaps. The `screen`/watchlist now **auto-pulls the
real PumpSwap price for graduated rows** so you never trust the frozen 411 again. What's
missing is only *continuous AMM ingestion for backtesting*. Treat graduation as the
learnable target and remember a graduate can dump immediately on PumpSwap — **and usually
does**: live **2026-06-28**, GTA graduated at ~$27.3k then bled to ~$5.9k (**−78%**) and a
FOMO token to ~$1.5k (**−94%**) on PumpSwap within minutes — smart bots long gone, retail
holding the bag (the exit-liquidity outcome, now visible).

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
≈ 5% round-trip drag. Peak is censored at graduation (~411 SOL) in the firehose; post-grad PumpSwap price is off-firehose but live-queryable via Helius (pool reserves).

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
trade real funds on them as-is.** The structural blockers (sub-second latency + PumpSwap AMM post-grad
data) remain hard prerequisites. Service stopped 2026-06-26. To revisit, adjust the `papertrade`
knobs and re-run — but expect the same unless the latency/PumpSwap AMM problems are solved first.
Ledger preserved at `data/paper_trades.jsonl`; re-summarize via meme-ops `paper-stats`.

**Two structural blockers to any real edge** (neither is a pattern tweak):
1. **Latency** — 5–15 min snapshot lag, and even a 60s decision is too slow vs first-second
   snipers. Needs sub-second live alerting/execution.
2. **Post-graduation blindness (firehose only)** — the big moves are on PumpSwap AMM, which the
   *firehose ingest* doesn't capture. BUT it IS live-queryable on-demand via **Helius RPC** (pool
   reserves / swaps) — so spot-checking a graduated token's real price is NOT blind (the watchlist
   auto-does it). What's missing is *continuous* AMM ingestion for backtesting post-grad EV.
Until BOTH are solved, **do not trade real funds.**

## 40-Minute Survivor Screening — the live buy-decision tool

This is the **primary use case**: *"a coin is ~30–45 min old — is it a good buy NOW?"* (NOT the
early-life graduation detector above; that one's signal is at 1–2 min and isn't tradeable.)

**Base rate (2026-06-26, full 24h; refined 2026-06-28):** "alive at 40 min" is definition-sensitive
— ~**15%** of launches have *any* trade after the 40-min mark, but only ~**1–2%** are *still actively
trading* then (≥10 trades / ≥10 fresh buyers in the 30–40 min window; trading-at-40min 1.65%, ≥10
trades 1.13%, ≥10 buyers 0.90% on 2026-06-28). The operative survivor pool the screener targets is that
rare ~1–2%, not 15% — this reconciles the earlier "~4%". Of survivors, only **~12% rise ≥50%** afterward
while most just bleed (median sits at the ~28 SOL launch, going nowhere). So **buying a *random* 40-min
survivor is a bad bet** — the tool's job is to pick the ~12% that run. Two signals do that, both
validated on two independent 24h windows:

HEURISTIC: Smart-money wallets (cheap-launch winners)
SIGNAL: a wallet that BUYS CHEAP AT LAUNCH (first 60s AND market cap ≤ 35 SOL) and whose such buys
graduate (peak ≥ 84) at a high rate over the rolling window. Threshold for the set: ≥10 cheap-launch
buys, ≥35–40% success. If one of these bought a token early → it's a strong bullish marker.
EVIDENCE: these wallets' cheap-launch picks succeed at ~**35–50% vs the ~12% base (≈2.4–4× lift)**.
Validated across 3 independent windows: split-half on two separate days (28.3% & 28.5% vs ~13% base);
a clean cross-day test — day-1's 31 wallets → day-2's 9,343 *brand-new* coins = **34.4% vs 14.1% base**;
AND a fresh **2026-06-28** forward test — an 18-wallet set frozen from the first 12h, applied to the next
12h's **9,353 brand-new** coins, graduated at **48.7% vs 11.9% base (4.1× lift, 55/113)** with a clean
dose-response (0 smart→11.5%, 1→47.8%, 2+→100%). The edge genuinely transfers forward. Reactive
*chasers* (buy high, after a 3×) look even better on paper but are USELESS — vet that a wallet buys
CHEAP, not just early.
CONFIDENCE: high (validated out-of-sample on 3 independent windows: 2026-06-26 ×2 + 2026-06-28).
**CRITICAL (2026-06-28): this is a LAUNCH-MINUTE signal, NOT a 40-min signal — the two DON'T overlap.**
Cross-tab on the test half: of 113 smart-backed coins, 91 had **no trade after 40 min** (graduated→migrated
to PumpSwap AMM→gone from pump.fun, or dead) and the 22 that lingered were all inactive with **0% upside**;
coins that were BOTH smart-backed AND still-active at 40 min = **zero**. Smart-backed winners graduate fast
and LEAVE before the 40-min decision point. ⇒ The screener's "rank live 40-min survivors by # smart early
buyers" is **near-empty in practice** — smart wallets are usable only as a *launch-time* (sub-60s) bet,
which needs sub-minute execution we don't have. The independent, executable 40-min edge is "Still-active"
below (and it does NOT depend on smart wallets).
DECAY: the set rots fast — only ~20 of 31 wallets stayed active one day later. **Recompute the smart
set continuously from the rolling 24h; never trust a stale list.** (This is why capture must run.)
ACTION: a 30–45-min token whose first-60s buyers include current smart wallets = surface it.

HEURISTIC: Still-active at 40 min (ongoing breadth)
SIGNAL: in the last ~10 min, the token is still pulling **≥10–15 distinct fresh buyers and ≥20 trades**.
EVIDENCE: survivors still active this way rise ≥50% at **~35% vs ~12% base (≈3× lift)**; ≥2× at ~20%
vs 6%. Dead-quiet survivors (≤2 buyers/10 min, the median loser) stay dead. Net flow does NOT matter —
*breadth/activity* does. **Revalidated 2026-06-28** (independent 24h): active survivors (≥15 fresh
buyers in the 30–40 min window, n=182) hit **36.3% ≥50% / 26.4% ≥2×** vs **3.4% / 1.8%** for quiet
ones (0–4 buyers, n=3,331); mid bucket (5–14, n=109) = 12.8%. Clean monotonic, ~10× active-vs-quiet.
CONFIDENCE: medium-high (validated across two independent 24h windows, 2026-06-26 & 2026-06-28).
ACTION: require ongoing activity; a flat survivor at launch mcap is a pass.

**The screener — `survivors` CLI / `screen_survivors` MCP (BUILT + deployed):** computes the
smart-wallet set fresh from the rolling 24h — where **smart = SELECTIVE winners**: cap distinct coins
traded ≤200 AND exclude the dev's own bundle-buy (`trader <> creator`); spray bots (one traded 8,012
coins!) and devs were inflating the counts. Then it takes the cohort created **30–45 min ago** and
surfaces **only those still ALIVE** (recent activity required — a dead coin is NEVER proposed, even
if smart money bought it early), ranked by # smart early buyers. Output: age, smart_early_buyers,
buyers/trades last 10 min, cur/peak mcap, last_trade_age_s.
REALITY CHECK (J2Fg "50", 2026-06-26): even **4 selective smart wallets** bought a coin that pumped
to ~$9.7k in 1 min then rugged by min 3 — the signal is **~35–40%, not a guarantee**. Data ingestion
was CORRECT (it captured the pump and death accurately); the fixes were definitional (exclude
bots/devs) + never surface dead coins.

**Honest limits:** best candidates still only hit ~35% — a ~3× better gamble, NOT a sure thing.
Needs live capture for a fresh wallet set. Multi-week wallet persistence still unproven. Research,
not financial advice.

## Bot Structure & Tradeability — Exhaustive Negative Result (2026-06-28)

User pushed (correctly) that pump.fun is bot-driven and asked whether the bots' repeating logic is
exploitable, including late-stage patterns and lead-lag *sequences* between bots. Ran a from-scratch,
unbiased sweep on the fresh frozen 24h (26,797 tokens / 1.83M trades), querying via **direct DuckDB over
SSH** (`python3 -m duckdb` 1.5.4 installed on the box, reading `hot.duckdb` read-only) because the MCP
path dropped mid-session. **Every one of the user's structural hypotheses was CONFIRMED; none yielded a
realizable long edge.**

**The market is ~100% bots.** First-buy latency median = **0 ms**; **98.5% of coins sniped within 1 s**
(98.3% within 200 ms). 23,014 early-snipe wallets, of which **1,728 recurring bots** (≥10 coins), 330
heavy (≥50), one wallet sniped **6,554** coins; recurring bots account for **66%** of all early snipes.

**Bot behaviour IS predictive (detection works everywhere):**
- Recurring bots in first 10 s → graduation: 0→7.5%, 1→10.4%, 2–3→17.5%, **7+→33.8%**.
- Unbiased feature-AUC scan (vs graduation): top axes are early **activity/volume/breadth** (AUC 0.83–0.93,
  but partly LEAKY = detecting an in-progress graduation) and **low whale concentration** (`whale_share`
  AUC ~0.11, strong INVERSE — one whale = death, broad buying = life). Specific wallets are NOT the top
  axis; breadth + low-whale + activity are.
- **Survival continuum** (forward ≥+50% rate, conditioned on "still actively traded", by entry age):
  2 min 6.9% → 10 min 24% → 20 min 33% → 60 min 37%. The edge is **survival / waiting out the corpses**,
  not any magic window — "40 min" is just one point on this curve.
- **Sequence & roles are real (the user's lead-lag idea):** "confirmer" wallets enter at **20–70 s,
  buy-rank 50–200, ~100% graduation** — they wait, evaluate, and only buy winners. Out-of-sample (set
  defined on 1st 12h → applied to 2nd 12h), *following* them gives **median forward PEAK +38.5%**, 42.9%
  reach +50%. The lead-lag chain exists and recurs.

**But NOTHING is realizable as profit.** Path-dependent sims (TP/SL + ~5% round-trip costs) across every
entry age (2–60 min) and every exit policy (TP +50%→+200%, wide/no stop, **and exit-on-the-bots'-own-
selling**):
- Best result anywhere = **−0.12%** (late-bot cohort, TP+200%/SL−50%), and NOT out-of-sample stable
  (−6% / +7% across halves).
- Follow-the-confirmer (the strongest forward signal): **−5.6%** best policy; median **final** outcome
  **−52%** while median **peak** was +38.5%.
- Universal shape: a brief **manufactured pop** (the predictable part) then a **dump**. You cannot catch
  the tick-top; any hold rides to ~−50%; a −30% stop is hit by the median trade *before* the pop.

**The unifying law (proven ~5 independent ways): the predictable thing is a manufactured spike you cannot
realize.** Detection works everywhere; profit nowhere — you are always **exit liquidity** for the bots
that generate the signal (early = behind by milliseconds; late = behind by detection lag; exit-on-bot-
sells = you necessarily sell *after* them). Fully consistent with the paper trial (−18.7 SOL).

**Honest status: there is NO realizable long edge in this dataset** (pump.fun, censored at graduation,
minutes-late). Structural, not a tuning miss. **Do not keep mining entry rules** — they will keep
surfacing predictable-but-unsellable spikes. The only theoretical money-path left is **execution-speed
scalping of the pop** (sub-second entry+exit to front-run the dump): an infra build, tiny edge (+38%
median peak, far less realizable) against −50% downside — not recommended. *Method note:* a zero-`mcap`
row artifact once made a `3×` take-profit trivially true and produced a fake **+5.7%**; always guard
`entry > 0` in TP/SL sims.

## Research Log

- **2026-06-28b — bot-structure & tradeability investigation (exhaustive negative).** See the section
  "Bot Structure & Tradeability" above. Market is ~100% bots (0 ms snipe, 98.5% <1 s; 1,728 recurring
  bots = 66% of early snipes). Coordination, late-stage accumulation, and lead-lag *confirmer* wallets
  are ALL predictive (confirmer-follow = +38.5% median forward peak, OOS) — but EVERY entry age × exit
  policy is realized-negative (best −0.12%, unstable; confirmer-follow −5.6%; median final −52% vs +38.5%
  peak). Conclusion: predictable manufactured spike, unsellable, you're exit liquidity → **no realizable
  long edge in this data; stop mining entries.** Only theoretical path = sub-second pop-scalping (infra,
  not recommended). Queried via direct DuckDB-over-SSH after MCP dropped; ingester stayed stopped+disabled.
- **2026-06-28 — survivor-screener re-validation (fresh independent 24h; ingester then stopped).**
  Frozen window: **26,797 tokens · 1,837,022 trades · 100,185 traders**; 25,315 mature launches
  (≥100 min observation), as-of 2026-06-28 ~09:55 (Helius firehose). **Base rates stable
  out-of-sample:** graduation (≥3×) **11.57%**, ≥10× **2.15%** (vs 11.0% / 2.08% on 2026-06-25).
  **Survival definition pinned down:** 14.7% have *any* trade after 40 min, but only ~1–2% are still
  *actively* trading (trading-at-40min 1.65%, ≥10 trades 30–40 min 1.13%, ≥10 buyers 0.90%) — this
  reconciles the earlier "~4%". **(1) Smart-money wallets VALIDATED forward:** 18-wallet set frozen
  from the first 12h (≥10 cheap-launch buys, ≥40% grad, ≤200 coins, trader≠creator) → applied to the
  next 12h's 9,353 brand-new coins = **48.7% graduate vs 11.9% base (4.1× lift, 55/113)**; dose-response
  0/1/2+ smart buyers = 11.5% / 47.8% / 100%. Confidence raised to **high** (3 independent windows).
  **(2) Still-active at 40 min VALIDATED:** active survivors (≥15 fresh buyers, 30–40 min, n=182) rise
  ≥50% at **36.3%** vs **3.4%** quiet (n=3,331) — ~3× the survivor base, ~10× vs quiet. Confidence →
  **medium-high** (2 windows). **Caveat unchanged:** these predict *graduation*, not profit — Trading
  Notes still govern (paper trial −18.7 SOL). Ingester `meme-expert-ingest` **stopped + disabled** after
  this run per user (Helius credit burn halted; data frozen at hot.duckdb/snapshot.duckdb — re-enable to
  resume capture & keep the smart-wallet set fresh).
- **2026-06-26 — 40-min survivor screening + smart-wallet mining.** Reframed to the real use case
  (judge a ~40-min-old coin as a buy), since the early-life detector isn't tradeable (paper trial
  lost −18.7 SOL / 1,915 trades, avg entry 71.7 SOL = bought after the move). Findings: (1) only
  ~4% of launches survive to 40 min; of those ~12% rise ≥50%, ~44% only bleed. (2) **Smart-money
  wallets** — wallets that buy cheap at launch (≤35 SOL, first 60s) and win — persist out-of-sample:
  split-half on two days (28.3% & 28.5% vs ~13% base) AND a clean cross-day test (day-1's 31 wallets
  → day-2's 9,343 *new* coins = 34.4% vs 14.1% base, ~2.4×). Set decays ~⅓/day → recompute live.
  Reactive chasers (buy high after a 3×) look great but are useless — vet for CHEAP buys. (3)
  **Still-active at 40 min** (≥10–15 fresh buyers / 10 min) → ~3× lift. Capture re-enabled to keep
  the wallet set fresh; building the `survivors` / `screen_survivors` screener.
- **2026-06-25 — first full-firehose 24h run** (Helius, 2026-06-23 21:07 → 06-24 21:17
  UTC). Dataset: **27,965 tokens · 1,894,214 trades · 98,897 distinct traders** — every
  pump.fun trade, no 500-sub cap (vs the frozen 100-min/1,318-labelable set). Findings:
  - **Outcome = graduation.** Peaks cluster hard at the **~411 SOL curve-completion cap**;
    "≥3×" ≈ "graduated." True base rate is **11.0%** (3,068/27,863), not the old sub-cap
    5.2%. ≥10× = 2.08%. Peak is censored at graduation — post-PumpSwap AMM life is invisible.
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
  - **TODOs:** (1) add PumpSwap AMM/post-graduation tracking to measure true exitability
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
