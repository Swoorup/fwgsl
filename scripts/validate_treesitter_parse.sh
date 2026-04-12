#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)

GRAMMAR_LIB=""
for candidate in \
  "$REPO_ROOT/tree-sitter-shadml/shadml.dylib" \
  "$REPO_ROOT/tree-sitter-shadml/shadml.so"
do
  if [[ -f "$candidate" ]]; then
    GRAMMAR_LIB="$candidate"
    break
  fi
done

if [[ -z "$GRAMMAR_LIB" ]]; then
  echo "missing built tree-sitter grammar: run \`mise run shadml:grammar:build\` first" >&2
  exit 1
fi

mapfile -t SHADML_FILES < <(
  cd "$REPO_ROOT"
  rg --files . -g '*.shadml' -g '!target/**' | sort
)

if [[ ${#SHADML_FILES[@]} -eq 0 ]]; then
  echo "no .shadml files found"
  exit 0
fi

echo "Parsing ${#SHADML_FILES[@]} shadml files with Tree-sitter..."

cd "$REPO_ROOT"
tree-sitter parse \
  --quiet \
  --lib-path "$GRAMMAR_LIB" \
  --lang-name shadml \
  "${SHADML_FILES[@]}"

echo "Tree-sitter parsed ${#SHADML_FILES[@]} shadml files successfully."
