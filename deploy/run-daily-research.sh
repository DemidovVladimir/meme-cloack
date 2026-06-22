#!/usr/bin/env bash
# Mac-side daily autonomous research. Runs Claude headlessly on your subscription,
# reading the REMOTE DuckDB over the SSH-stdio MCP server, discovering patterns,
# updating .claude/skills/meme-expert/SKILL.md, and committing.
#
# Scheduled by deploy/com.meme-expert.daily.plist (launchd). Run manually to test.
set -euo pipefail

# Repo root = parent of this script's dir.
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

if [ ! -f .mcp.json ]; then
  echo "missing .mcp.json — copy .mcp.json.example and set your server host" >&2
  exit 1
fi

LOG_DIR="$REPO/logs"
mkdir -p "$LOG_DIR"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"

claude -p "$(cat deploy/research-prompt.md)" \
  --mcp-config .mcp.json \
  --allowedTools "Read,Edit,mcp__meme-expert,Bash(git add:*),Bash(git commit:*),Bash(git status:*),Bash(git diff:*)" \
  --permission-mode acceptEdits \
  --output-format json \
  > "$LOG_DIR/research-$STAMP.json" 2> "$LOG_DIR/research-$STAMP.err"

echo "daily research complete -> $LOG_DIR/research-$STAMP.json"
