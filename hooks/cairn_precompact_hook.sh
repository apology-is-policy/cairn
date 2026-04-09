#!/usr/bin/env bash
# cairn_precompact_hook.sh — emergency flush before context compaction
# Called by Claude Code's PreCompact hook.

set -euo pipefail

CAIRN_DB="${CAIRN_DB:-$HOME/.cairn/cairn.db}"
SESSION_ID="${CAIRN_SESSION_ID:-sess_$(date +%Y%m%d_%H%M%S)}"

if [ ! -d "$CAIRN_DB" ]; then
  exit 0
fi

cairn-cli --db "$CAIRN_DB" \
  checkpoint \
  --session-id "$SESSION_ID" \
  --emergency \
  2>>"$HOME/.cairn/logs/hook.log" || true
