# Deploy

Two machines, no Pulumi, no service zoo:

- **Hetzner box:** the ingester + DuckDB + MCP binary. No Claude, no Anthropic auth.
- **Your Mac:** the daily Claude research (your subscription) + interactive use.
- **Bridge:** MCP-over-SSH stdio — your Mac launches the MCP server *on the box*
  over SSH and pipes its stdio back. No exposed port, no extra auth.

## 1. Server (Hetzner / Ubuntu 24.04)

Build the binary (on the box, or build on your Mac for the right target and scp it):

```bash
sudo apt-get update && sudo apt-get install -y build-essential cmake git curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && . "$HOME/.cargo/env"
git clone <your repo> meme-expert && cd meme-expert
cargo build --release
sudo install -m755 target/release/meme-expert /usr/local/bin/meme-expert
```

Create the service user, data dir, and config:

```bash
sudo useradd -r -m -d /home/meme meme
sudo -u meme mkdir -p /home/meme/meme-expert/data
sudo -u meme git clone <your repo> /home/meme/meme-expert        # for .env + .claude
sudoedit /home/meme/meme-expert/.env                              # set PUMPPORTAL_API_KEY,
#   MEME_DB_PATH=/home/meme/meme-expert/data/hot.duckdb
#   MEME_SNAPSHOT_PATH=/home/meme/meme-expert/data/snapshot.duckdb
sudo chmod 600 /home/meme/meme-expert/.env
```

Install + start the ingester:

```bash
sudo cp deploy/meme-expert-ingest.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now meme-expert-ingest
journalctl -u meme-expert-ingest -f          # watch "sweep tracked=N tokens_seen=N trades_seen=N"
sudo -u meme MEME_SNAPSHOT_PATH=/home/meme/meme-expert/data/snapshot.duckdb meme-expert stats
```

`trades_seen` stays 0 if `PUMPPORTAL_API_KEY` is unset or the linked wallet is
underfunded (PumpPortal logs "Minimum balance not met...").

### Cost control

The paid trade stream is metered. The 40-minute survivor policy unsubscribes dead
tokens within minutes, and `MAX_ACTIVE_TRADE_SUBS` (default 500) caps concurrent
subscriptions. Lower `MAX_ACTIVE_TRADE_SUBS` and/or `SURVIVOR_AGE_MINUTES`, or
raise `DEATH_DRAWDOWN_PCT` / lower `DEATH_SILENCE_MINUTES`, to spend less. Watch
`trades_seen` per sweep in the journal as your cost proxy.

## 2. Your Mac

```bash
git clone <your repo> meme-expert && cd meme-expert
cp .mcp.json.example .mcp.json        # set "meme@YOUR_SERVER_IP"
```

Confirm the SSH-stdio MCP server works (Claude runs the server on the box):

```bash
claude mcp list                        # meme-expert should be healthy
claude -p "call window_stats" --mcp-config .mcp.json --allowedTools "mcp__meme-expert"
```

(Make sure `ssh meme@YOUR_SERVER_IP` works key-only first.)

### Daily research (launchd)

```bash
# edit the repo path + hour in the plist, then:
cp deploy/com.meme-expert.daily.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.meme-expert.daily.plist
launchctl kickstart -k gui/$(id -u)/com.meme-expert.daily      # test-fire now
git -C . log --oneline -- .claude/skills/meme-expert/SKILL.md  # should show a new commit
```

It runs `deploy/run-daily-research.sh`: Claude reads the remote 24h data over MCP,
discovers patterns, updates `.claude/skills/meme-expert/SKILL.md`, and commits.
Logs land in `logs/research-*.json`.

### Interactive screening

```bash
cd meme-expert && claude
> "Use the meme-expert skill and the meme-expert tools to screen tokens created
   in the last 30 minutes against our heuristics and rank the best few."
```

## Stop / teardown

```bash
sudo systemctl disable --now meme-expert-ingest        # stops capture (and spend)
launchctl bootout gui/$(id -u)/com.meme-expert.daily   # stops daily research
```

Raw data is disposable (24h rolling). The durable asset is `SKILL.md` in git.
