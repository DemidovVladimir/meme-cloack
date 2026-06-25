---
name: meme-ops
description: Operate and inspect the meme-expert Hetzner server. Use when asked for server "state" — how much memory/disk is left and how many Helius credits are left/burning — or to check service health, how much data is in the DB, disk/memory/CPU, recent logs, or to start/stop/restart a service (ingester or papertrade). A runbook of SSH commands against the deployment — distinct from the meme-expert skill (which screens tokens via MCP).
allowed-tools: Bash, Read
---

# Meme-Ops — meme-expert server runbook

Operational commands for the live deployment. Run these to answer "is it healthy?",
"how much data / disk?", "how much is it spending?", "what's in the logs?". The
intelligence/screening lives in the separate **meme-expert** skill; this one is
plumbing.

## The deployment (facts)

| | |
|---|---|
| Server | Hetzner Cloud `meme-expert` (id `144038400`, cpx22, nbg1, Ubuntu 26.04) |
| IP | `178.104.2.95` |
| SSH key | `~/.ssh/hetzner_meme_ed25519` |
| Users | `root` (ops/full) · `meme` (service user: runs ingester, owns the data) |
| Services | `meme-expert-papertrade` (active 2026-06-25+ — paper-trading, the current live Helius consumer) · `meme-expert-ingest` (stopped+disabled 2026-06-25; data capture — re-enable to resume) |
| Binary | `/usr/local/bin/meme-expert` (subcommands: `ingest` `mcp` `stats` `prune` `screen` `papertrade`) |
| Data dir | `/home/meme/meme-expert/data/` |
| DBs | `hot.duckdb` (live, held read-write by ingester) · `snapshot.duckdb` (read-only, refreshed every snapshot interval) · `.wal` is the active write-ahead log |
| Config | `/home/meme/meme-expert/.env` (root-readable only) |
| Source | `INGEST_SOURCE` in `.env`: `helius_ws` (Helius LaserStream WS — flat cost, complete) or `pumpportal` (legacy metered fallback) |

**Under `INGEST_SOURCE=helius_ws`** the trade stream is flat-cost (no per-trade SOL
spend, no `MAX_ACTIVE_TRADE_SUBS` cap) — the "PumpPortal cost proxy" section below is
N/A. The sweep log line reads `tracked_tokens=… tokens_seen=… trades_seen=… trades_kept=…`.
Health checks (`grep "sweep "`, `systemctl`, `stats`, `df`) are unchanged. The MCP gains
a `screen_candidates` tool (8 tools total).

Shorthand used below — `SSH=ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95`
(use `meme@` instead of `root@` for the `stats`/`mcp` read path). Commands are
written out in full so each runs standalone.

## Quick health check (one shot)

```bash
ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 'bash -s' <<'EOF'
echo "== service ==";   systemctl is-active meme-expert-ingest; systemctl is-enabled meme-expert-ingest
echo "== uptime/load =="; uptime
echo "== latest sweep =="; journalctl -u meme-expert-ingest -o cat | grep "sweep " | tail -1 | sed -E 's/\x1b\[[0-9;]*m//g'
echo "== data size ==";  du -sh /home/meme/meme-expert/data; ls -lh /home/meme/meme-expert/data
echo "== disk ==";       df -h / | awk 'NR==1||/\/$/'
echo "== mem ==";        free -h | awk '/Mem:|Swap:/'
EOF
```

## State — memory/disk left + Helius credit burn

One-shot for "what's the server state": RAM + disk left, which Helius consumers are live,
and an *estimate* of credits each burned since (re)start. One full pump.fun firehose ≈
**467 credits/min ≈ ~6.7%/day** of the 10M Developer plan. The exact remaining-credits figure
is **not exposed to the RPC api-key** (the admin API needs a separate Bearer admin token —
probed: the RPC key 401s), so read the dashboard for the real number.

```bash
ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 'bash -s' <<'EOF'
echo "== memory =="; free -h | awk '/Mem:|Swap:/'
echo "== disk =="; df -h / | awk 'NR==1||/\/$/'
echo "== Helius consumers (each ~467 cr/min ~= 6.7%/day of 10M) =="
now=$(date +%s)
for svc in meme-expert-papertrade meme-expert-ingest; do
  st=$(systemctl is-active "$svc")
  line="$svc: $st"
  if [ "$st" = active ]; then
    start=$(systemctl show "$svc" -p ActiveEnterTimestamp --value)
    up=$(( now - $(date -d "$start" +%s) ))
    line="$line  up=$((up/3600))h$(((up%3600)/60))m  est_credits_since_start~=$(( up/60 * 467 ))"
  fi
  echo "$line"
done
echo "Exact credits remaining: https://dashboard.helius.dev  (Developer plan = 10,000,000/cycle)"
EOF
```

`est_credits_since_start` is a rolling-rate estimate per service, **not** the billed cycle
total (the dashboard counter is cumulative across the whole monthly cycle and resets monthly).
For exact remaining credits, open the dashboard — or paste a screenshot and I'll read it.

*Optional — exact number via API:* `GET https://admin-api.helius.xyz/v0/usage` with header
`Authorization: Bearer <ADMIN_TOKEN>` returns billed usage. The RPC api-key is NOT an admin
token; get one via `helius login` (CLI) or by copying the dashboard's `Authorization` header
from browser devtools, store it as `HELIUS_ADMIN_TOKEN`, then curl it.

## Paper-trade results (`paper-stats` — the gate metric)

The paper trader appends every closed trade to `data/paper_trades.jsonl` (append-only, survives
restarts) — that file is the **durable source of truth**. This summarizes it on demand, independent
of any cron/session. Net P&L is already after fees+slippage (0.1 SOL/position). The journald
`summary` line is only a live convenience counter and resets if the service restarts; trust this.

```bash
ssh -i ~/.ssh/hetzner_meme_ed25519 meme@178.104.2.95 'python3 - /home/meme/meme-expert/data/paper_trades.jsonl' <<'PY'
import sys, json, statistics
from collections import Counter
rows=[json.loads(l) for l in open(sys.argv[1]) if l.strip()]
n=len(rows)
if not n: print("no closed trades yet"); sys.exit()
fr=[r["pnl_frac"] for r in rows]; wins=sum(1 for x in fr if x>0)
reasons=Counter(r["reason"] for r in rows)
print(f"closed={n}  win_rate={100*wins/n:.1f}%  total_pnl_sol={sum(r['pnl_sol'] for r in rows):+.4f}")
print(f"mean/median per trade: {100*statistics.mean(fr):+.2f}% / {100*statistics.median(fr):+.2f}%")
print(f"avg entry mcap={statistics.mean(r['entry_mcap'] for r in rows):.1f} SOL  avg hold={statistics.mean(r['hold_s'] for r in rows):.0f}s")
for rsn,c in reasons.most_common():
    sub=[r["pnl_frac"] for r in rows if r["reason"]==rsn]
    print(f"  {rsn:12s} n={c:3d} mean {100*statistics.mean(sub):+.1f}%")
PY
```

**How SKILL.md gets updated:** `paper_trades.jsonl` is the raw ledger; the meme-expert skill's
"## Trading Notes" is the *curated* summary. Run `paper-stats` anytime → fold the numbers into
Trading Notes + commit. (The session-only analysis cron is only a best-effort auto-attempt; the
ledger + this command are the reliable path, so nothing is lost if the cron never fires.)

## Is there data? How much?

```bash
# Snapshot view (tokens / trades / distinct traders + top tokens). Refreshes every 5 min.
ssh -i ~/.ssh/hetzner_meme_ed25519 meme@178.104.2.95 \
  'MEME_SNAPSHOT_PATH=/home/meme/meme-expert/data/snapshot.duckdb /usr/local/bin/meme-expert stats'

# Live counters straight from the ingester (cumulative since start): tokens_seen, trades_seen,
# tracked (currently subscribed), dropped_dead (early rugs filtered), survivors (>40 min).
ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 \
  'journalctl -u meme-expert-ingest -o cat | grep "sweep " | tail -3 | sed -E "s/\x1b\[[0-9;]*m//g"'
```

`survivors` stays 0 for the first ~40 min after (re)start — a survivor is a token
that lived 40 minutes. `snapshot.duckdb` lags the live DB by up to 5 min.

## PumpPortal cost proxy

The paid trade stream is metered. Watch `trades_seen` growth per sweep and
`active_subs` (capped by `MAX_ACTIVE_TRADE_SUBS`, default 500). Rising `dropped_dead`
is the 40-min early-rug filter doing its job (less spend).

```bash
ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 \
  'journalctl -u meme-expert-ingest -o cat | grep "sweep " | tail -8 | sed -E "s/\x1b\[[0-9;]*m//g"'
```

## Disk / memory / CPU

```bash
ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 \
  'df -h /; echo; du -sh /home/meme/meme-expert/data /root/meme-expert/target /root/.cargo /root/.rustup 2>/dev/null; echo; free -h; echo; top -bn1 | head -12'
```

The DB is a rolling 24h window (old rows pruned every 5 min), so data size plateaus.
Big non-data users are build leftovers: `/root/meme-expert/target` (~1.1 GB, safe to
delete — binary is already installed) and the Rust toolchain `~/.cargo`+`~/.rustup`
(~1.7 GB, keep if you want fast rebuilds).

## Logs

```bash
# Follow live:
ssh -t -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 'journalctl -u meme-expert-ingest -f'
# Last N / errors only:
ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 'journalctl -u meme-expert-ingest -n 50 --no-pager'
ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 'journalctl -u meme-expert-ingest -p warning --no-pager -n 50'
```

## MCP check (Mac → server path)

```bash
# Lists the 7 read-only tools = the .mcp.json path works end-to-end.
printf '%s\n%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
| ssh -i ~/.ssh/hetzner_meme_ed25519 meme@178.104.2.95 \
  'timeout 10 env MEME_SNAPSHOT_PATH=/home/meme/meme-expert/data/snapshot.duckdb /usr/local/bin/meme-expert mcp 2>/dev/null' \
| tr ',' '\n' | grep '"name":"'
```

## Control (start / stop / restart / spend)

```bash
ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 'systemctl restart meme-expert-ingest'
ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 'systemctl stop meme-expert-ingest'      # stops capture AND PumpPortal spend
ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 'systemctl start meme-expert-ingest'
```

To change tuning (`MAX_ACTIVE_TRADE_SUBS`, `SURVIVOR_AGE_MINUTES`, `RETENTION_HOURS`,
etc.): edit `/home/meme/meme-expert/.env` (as root), then `systemctl restart meme-expert-ingest`.

## Redeploy after a code change

```bash
ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 'bash -lc "
  cd /root/meme-expert && git pull -q && . \$HOME/.cargo/env &&
  cargo build --release && install -m755 target/release/meme-expert /usr/local/bin/meme-expert"'
ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 'systemctl restart meme-expert-ingest'
```

## Extend this runbook

Add new checks as fenced `bash` blocks under a clear `##` heading, following the
pattern above (full `ssh -i ~/.ssh/hetzner_meme_ed25519 <user>@178.104.2.95 '<cmd>'`).
Use `root@` for system/service/disk, `meme@` for `stats`/`mcp` (read-only data path).
Keep destructive actions (restart/stop, `prune`, file edits) in clearly-labelled
sections.
