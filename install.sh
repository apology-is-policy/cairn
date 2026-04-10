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

echo "Building Cairn ($PROFILE profile)..."
cd "$REPO_ROOT"
cargo build $CARGO_FLAGS -p cairn-cli -p cairn-mcp

mkdir -p "$BIN_DIR"

for bin in cairn-cli cairn-mcp; do
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
echo "Done."

if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
  echo
  echo "WARNING: $BIN_DIR is not on your PATH."
  echo "Add this to your shell config (~/.zshrc, ~/.bashrc, etc.):"
  echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
fi
