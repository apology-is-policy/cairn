#!/usr/bin/env bash
# cairn_save_hook.sh — periodic checkpoint
# Called by Claude Code's Stop hook.

set -euo pipefail

CAIRN_DB="${CAIRN_DB:-$HOME/.cairn/cairn.db}"
SESSION_ID="${CAIRN_SESSION_ID:-sess_$(date +%Y%m%d_%H%M%S)}"

if [ ! -d "$CAIRN_DB" ]; then
  exit 0  # No graph, nothing to do
fi

cairn-cli --db "$CAIRN_DB" \
  checkpoint \
  --session-id "$SESSION_ID" \
  2>>"$HOME/.cairn/logs/hook.log" || true
