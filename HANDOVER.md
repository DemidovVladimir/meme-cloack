# Handover — meme-expert

_Last updated: 2026-06-22 (evening), after first deploy._

## TL;DR

Built, deployed, and **live**. The ingester is capturing PumpPortal launches +
trades into a rolling 24h DuckDB on a Hetzner box; MCP-over-SSH works from this
Mac; the daily pattern-research job is scheduled. Nothing is broken. The items
below are follow-ups and the "tomorrow morning" routine, not blockers.

## What's running

| | |
|---|---|
| Server | Hetzner `meme-expert` — `178.104.2.95` (cpx22, nbg1, Ubuntu 26.04, id `144038400`) |
| Ingester | systemd `meme-expert-ingest`, **active + enabled** (auto-restart, starts on boot) |
| SSH key | `~/.ssh/hetzner_meme_ed25519` (users: `root` ops, `meme` service) |
| MCP | `.mcp.json` in repo → `ssh meme@178.104.2.95 … meme-expert mcp` (verified, 7 tools) |
| Daily research | launchd `com.meme-expert.daily` → fires **09:30 daily**, updates `SKILL.md`, commits |
| Repo | `github.com/DemidovVladimir/meme-cloack` (PUBLIC), branch `main` |
| Skills | `meme-expert` (screen tokens via MCP) · `meme-ops` (server runbook) |

As of last check: ~500 tokens / ~21k trades captured, 36 MB data, disk 13%, RAM fine.

## Open items

- [ ] **Revoke the Hetzner API token** — Cloud → Security → API Tokens → delete.
      Deploy is done; my local copy is gone, but the token still lives in your account. _(security, do this)_
- [ ] **Commit the new files** — `.claude/skills/meme-ops/SKILL.md` and this
      `HANDOVER.md` are untracked. (`.mcp.json` is gitignored — leave it.)
- [ ] **Confirm the first daily research run** (after 09:30 tomorrow) actually
      produced a `SKILL.md` commit (see runbook below).
- [ ] **Watch PumpPortal spend** — `trades_seen` / `active_subs` per sweep is the
      cost proxy. Lower `MAX_ACTIVE_TRADE_SUBS` in the server `.env` if it climbs.
- [ ] _(optional)_ Reclaim ~1.1 GB: `ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 'rm -rf /root/meme-expert/target'` (binary already installed; only needed if you won't rebuild soon).
- [ ] _(optional)_ Make the repo **private** if you don't want `SKILL.md` heuristics public.

## Tomorrow morning — routine

### 1. Did the daily research run and learn anything?
```bash
cd /Users/vladimirdemidov/development/meme_cloack
git log --oneline -5 -- .claude/skills/meme-expert/SKILL.md   # expect a new commit dated 06-23
ls -lt logs/research-*.json | head                            # run output
launchctl print "gui/$(id -u)/com.meme-expert.daily" | grep -E 'last exit code|state'
```
If it didn't fire (e.g. Mac asleep at 09:30), force one:
```bash
launchctl kickstart -k "gui/$(id -u)/com.meme-expert.daily"
# or run it in the foreground to watch:
deploy/run-daily-research.sh
```

### 2. Start the MCP and check for patterns (interactive)
The MCP server is **remote** — `claude` launches it on the box over SSH via
`.mcp.json`, no local daemon to start. Just open Claude from the repo:
```bash
cd /Users/vladimirdemidov/development/meme_cloack
claude
```
Then, in the session:
```
Use the meme-expert skill and the meme-expert MCP tools to screen tokens created in
the last 30 minutes against our heuristics, and rank the best few with reasons.
```
Other useful asks: _"call window_stats"_, _"top_tokens for the last hour"_,
_"token_detail + token_trades for <mint>"_, _"wallet_activity for <wallet>"_.

Headless one-liner (no chat):
```bash
claude -p "Call window_stats, then top_tokens for the last 2h. Summarize." \
  --mcp-config .mcp.json --allowedTools "mcp__meme-expert"
```

> Note: by morning there will be real **survivors** (tokens that lived 40 min) and a
> ~13h data window, so screening will actually be meaningful — tonight it's too
> early (survivors=0).

### 3. Check server state (the `meme-ops` skill)
In a Claude session, ask _"check the meme-expert server state"_ (routes to the
`meme-ops` skill), or run its one-shot directly:
```bash
ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 'bash -s' <<'EOF'
systemctl is-active meme-expert-ingest
journalctl -u meme-expert-ingest -o cat | grep "sweep " | tail -1 | sed -E 's/\x1b\[[0-9;]*m//g'
du -sh /home/meme/meme-expert/data; df -h / | awk '/\/$/'
EOF
```

## Gotchas / notes

- **Skills load at session start** — start a *fresh* `claude` session for
  `meme-ops` / updated `meme-expert` to be picked up.
- `snapshot.duckdb` (what `stats`/MCP read) lags the live DB by up to 5 min.
- Time columns are **epoch-millis** (`created_ms`, `ts_ms`) — no SQL `now()`/intervals.
- `survivors=0` for the first ~40 min after any ingester restart — expected.
- To stop capture **and** PumpPortal spend:
  `ssh -i ~/.ssh/hetzner_meme_ed25519 root@178.104.2.95 'systemctl stop meme-expert-ingest'`
- Full runbook (logs, cost, restart, redeploy, tuning): `.claude/skills/meme-ops/SKILL.md`.
- Deploy/architecture detail: `docs/DEPLOY.md`, `docs/ARCHITECTURE.md`.
