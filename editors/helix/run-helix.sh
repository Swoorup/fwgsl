#!/usr/bin/env bash
# Launch Helix with shadml language support using a temporary config directory.
# Usage: ./editors/helix/run-helix.sh [file.shadml ...]
#
# Prerequisites (handled by mise):
#   - shadml-lsp binary (built by `mise run release`)
#   - shadml.dylib grammar (built by `mise run grammar:build`)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
HX_DIR="/tmp/shadml-hx"
CONFIG_DIR="$HX_DIR/helix"
RUNTIME_DIR="$HX_DIR/helix/runtime"

# -- Set up config directory --------------------------------------------------

mkdir -p "$CONFIG_DIR"
mkdir -p "$RUNTIME_DIR/queries/shadml"
mkdir -p "$RUNTIME_DIR/grammars"

# Copy the user's existing Helix config as a base (themes, keymaps, etc.)
USER_HX_CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/helix"
if [ -d "$USER_HX_CONFIG" ]; then
  # Copy config files but not runtime/ (we manage runtime ourselves)
  for f in "$USER_HX_CONFIG"/*.toml "$USER_HX_CONFIG"/*.scm; do
    [ -f "$f" ] && { rm -f "$CONFIG_DIR/$(basename "$f")"; cp "$f" "$CONFIG_DIR/$(basename "$f")"; }
  done
  # Copy themes directory if present
  if [ -d "$USER_HX_CONFIG/themes" ]; then
    rm -rf "$CONFIG_DIR/themes"
    cp -r "$USER_HX_CONFIG/themes" "$CONFIG_DIR/themes"
  fi
fi

# Copy languages.toml (overrides the user's copy with shadml support)
rm -f "$CONFIG_DIR/languages.toml"
cp "$REPO_ROOT/editors/helix/languages.toml" "$CONFIG_DIR/languages.toml"

# Symlink query files (from tree-sitter-shadml if available, else editors/helix)
if [ -d "$REPO_ROOT/tree-sitter-shadml/queries" ]; then
  QUERY_SRC="$REPO_ROOT/tree-sitter-shadml/queries"
else
  QUERY_SRC="$REPO_ROOT/editors/helix/queries"
fi

for f in "$QUERY_SRC"/*.scm; do
  [ -f "$f" ] && ln -sf "$f" "$RUNTIME_DIR/queries/shadml/$(basename "$f")"
done

# -- Copy pre-built grammar ---------------------------------------------------

GRAMMAR_DIR="$RUNTIME_DIR/grammars"
if [ -f "$REPO_ROOT/tree-sitter-shadml/shadml.dylib" ]; then
  rm -f "$GRAMMAR_DIR/shadml.dylib" "$GRAMMAR_DIR/shadml.so"
  cp "$REPO_ROOT/tree-sitter-shadml/shadml.dylib" "$GRAMMAR_DIR/shadml.dylib"
  cp "$REPO_ROOT/tree-sitter-shadml/shadml.dylib" "$GRAMMAR_DIR/shadml.so"
elif [ -f "$REPO_ROOT/tree-sitter-shadml/shadml.so" ]; then
  rm -f "$GRAMMAR_DIR/shadml.so" "$GRAMMAR_DIR/shadml.dylib"
  cp "$REPO_ROOT/tree-sitter-shadml/shadml.so" "$GRAMMAR_DIR/shadml.so"
  cp "$REPO_ROOT/tree-sitter-shadml/shadml.so" "$GRAMMAR_DIR/shadml.dylib"
fi

# -- Launch Helix -------------------------------------------------------------

LSP_BIN="$REPO_ROOT/target/release/shadml-lsp"
export PATH="$REPO_ROOT/target/release:$PATH"
export XDG_CONFIG_HOME="$HX_DIR"

echo "Config:  $CONFIG_DIR/languages.toml"
echo "Runtime: $RUNTIME_DIR"
echo "LSP:     $LSP_BIN"
echo ""

exec hx "$@"
