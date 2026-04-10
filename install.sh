#!/usr/bin/env bash
# install.sh — build and install Cairn binaries to ~/.local/bin/
#
# Usage:
#   ./install.sh           # build release and install
#   ./install.sh --debug   # use debug build instead (faster compile, slower runtime)

set -euo pipefail

PROFILE="release"
PROFILE_DIR="release"
CARGO_FLAGS="--release"

if [[ "${1:-}" == "--debug" ]]; then
  PROFILE="debug"
  PROFILE_DIR="debug"
  CARGO_FLAGS=""
fi

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
BIN_DIR="$HOME/.local/bin"
HOOKS_DIR="$HOME/.cairn/hooks"
LOGS_DIR="$HOME/.cairn/logs"

echo "Building Cairn ($PROFILE profile)..."
cd "$REPO_ROOT"
cargo build $CARGO_FLAGS -p cairn-cli -p cairn-mcp -p cairn-server

mkdir -p "$BIN_DIR"

for bin in cairn-cli cairn-mcp cairn-server; do
  src="$REPO_ROOT/target/$PROFILE_DIR/$bin"
  dst="$BIN_DIR/$bin"
  if [[ ! -f "$src" ]]; then
    echo "ERROR: expected binary not found: $src" >&2
    exit 1
  fi
  cp "$src" "$dst"
  echo "  installed $dst"
done

echo
echo "Installing hook scripts..."
mkdir -p "$HOOKS_DIR" "$LOGS_DIR"
for hook in cairn_save_hook.sh cairn_precompact_hook.sh; do
  src="$REPO_ROOT/hooks/$hook"
  dst="$HOOKS_DIR/$hook"
  cp "$src" "$dst"
  chmod +x "$dst"
  echo "  installed $dst"
done

# Stop any running daemon so the next client invocation picks up the new binary.
echo
echo "Stopping any running cairn-server processes..."
if pkill -TERM -f 'cairn-server --db' 2>/dev/null; then
  echo "  sent SIGTERM to existing cairn-server(s)"
  for i in 1 2 3 4 5 6 7 8 9 10; do
    if ! pgrep -f 'cairn-server --db' >/dev/null 2>&1; then
      break
    fi
    sleep 0.2
  done
fi

echo
echo "Done."

if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
  echo
  echo "WARNING: $BIN_DIR is not on your PATH."
  echo "Add this to your shell config (~/.zshrc, ~/.bashrc, etc.):"
  echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
fi
