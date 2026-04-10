#!/usr/bin/env bash
# cairn_precompact_hook.sh — emergency flush before context compaction
# Called by Claude Code's PreCompact hook.

set -euo pipefail

SESSION_ID="${CAIRN_SESSION_ID:-sess_$(date +%Y%m%d_%H%M%S)}"

# Find cairn-cli. Hooks fire from Claude Code's environment which may not
# inherit the user's interactive shell PATH, so try common locations.
CAIRN_CLI="${CAIRN_CLI:-}"
if [ -z "$CAIRN_CLI" ]; then
  if command -v cairn-cli >/dev/null 2>&1; then
    CAIRN_CLI="cairn-cli"
  elif [ -x "$HOME/.local/bin/cairn-cli" ]; then
    CAIRN_CLI="$HOME/.local/bin/cairn-cli"
  else
    exit 0  # cairn-cli not found, can't checkpoint
  fi
fi

# Discover the cairn db. Honor an explicit CAIRN_DB if set, otherwise walk
# up from cwd looking for a .cairn directory. If nothing is found, exit
# silently — we don't want a hook to create a fresh graph in a random repo.
if [ -n "${CAIRN_DB:-}" ]; then
  DB="$CAIRN_DB"
else
  DB=""
  d="$PWD"
  while :; do
    if [ -d "$d/.cairn" ]; then
      DB="$d/.cairn/cairn.db"
      break
    fi
    [ "$d" = "/" ] && break
    d="$(dirname "$d")"
  done
fi

if [ -z "$DB" ] || [ ! -d "$DB" ]; then
  exit 0  # No graph in this tree, nothing to do
fi

mkdir -p "$HOME/.cairn/logs"

"$CAIRN_CLI" --db "$DB" \
  checkpoint \
  --session-id "$SESSION_ID" \
  --emergency \
  2>>"$HOME/.cairn/logs/hook.log" || true
