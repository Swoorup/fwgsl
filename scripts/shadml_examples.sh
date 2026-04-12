#!/usr/bin/env bash
# Format, check formatting, or validate (format + compile) all .shadml example files.
#
# Usage:
#   ./scripts/shadml_examples.sh fmt          # format all files in-place
#   ./scripts/shadml_examples.sh fmt-check    # check all files are formatted (exit 1 if not)
#   ./scripts/shadml_examples.sh validate     # format each file, then compile the result

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)

# ---------------------------------------------------------------------------
# Resolve the shadml CLI binary (as an array to handle multi-word commands)
# ---------------------------------------------------------------------------
if command -v shadml &>/dev/null; then
  SHADML=(shadml)
else
  # Fall back to cargo run
  SHADML=(cargo run -p shadml_cli --quiet --)
fi

# ---------------------------------------------------------------------------
# Discover all .shadml files (same scope as grammar:parse-all)
# ---------------------------------------------------------------------------
mapfile -t SHADML_FILES < <(
  cd "$REPO_ROOT"
  find examples fixtures prelude -name '*.shadml' -type f 2>/dev/null | sort
)

if [[ ${#SHADML_FILES[@]} -eq 0 ]]; then
  echo "No .shadml files found."
  exit 0
fi

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
has_imports() {
  # Quick check: does the file contain a top-level import declaration?
  grep -qE '^\s*import ' "$1"
}

# Colours (disabled when stdout is not a terminal)
if [[ -t 1 ]]; then
  RED='\033[0;31m'
  GREEN='\033[0;32m'
  YELLOW='\033[0;33m'
  BOLD='\033[1m'
  RESET='\033[0m'
else
  RED='' GREEN='' YELLOW='' BOLD='' RESET=''
fi

# ---------------------------------------------------------------------------
# Mode: fmt -- format all files in-place
# ---------------------------------------------------------------------------
mode_fmt() {
  local count=0
  local changed=0
  for file in "${SHADML_FILES[@]}"; do
    local path="$REPO_ROOT/$file"
    local original
    original=$(<"$path")
    local formatted
    formatted=$( cd "$REPO_ROOT" && "${SHADML[@]}" fmt "$file" )
    if [[ "$original" != "$formatted" ]]; then
      printf '%s\n' "$formatted" > "$path"
      echo -e "  ${YELLOW}formatted${RESET}  $file"
      changed=$((changed + 1))
    fi
    count=$((count + 1))
  done
  echo -e "${BOLD}Formatted $count file(s) ($changed changed).${RESET}"
}

# ---------------------------------------------------------------------------
# Mode: fmt-check -- verify all files are already formatted
# ---------------------------------------------------------------------------
mode_fmt_check() {
  local count=0
  local unformatted=0
  for file in "${SHADML_FILES[@]}"; do
    local path="$REPO_ROOT/$file"
    local original
    original=$(<"$path")
    local formatted
    formatted=$( cd "$REPO_ROOT" && "${SHADML[@]}" fmt "$file" )
    if [[ "$original" != "$formatted" ]]; then
      echo -e "  ${RED}unformatted${RESET}  $file"
      unformatted=$((unformatted + 1))
    fi
    count=$((count + 1))
  done

  if [[ $unformatted -gt 0 ]]; then
    echo -e "${RED}${BOLD}$unformatted of $count file(s) are not formatted.${RESET}"
    echo "Run 'mise run shadml:fmt' to fix."
    exit 1
  fi

  echo -e "${GREEN}${BOLD}All $count file(s) are formatted.${RESET}"
}

# ---------------------------------------------------------------------------
# Mode: validate -- format each file, then compile the formatted result
# ---------------------------------------------------------------------------
mode_validate() {
  tmpdir=$(mktemp -d)
  trap 'rm -rf "$tmpdir"' EXIT

  local count=0
  local skipped=0
  local passed=0
  local failed=0
  local failures=()

  for file in "${SHADML_FILES[@]}"; do
    local path="$REPO_ROOT/$file"

    # Skip files with import declarations (they need bundle, not single-file compile)
    if has_imports "$path"; then
      echo -e "  ${YELLOW}skip${RESET}  $file  (has imports)"
      skipped=$((skipped + 1))
      count=$((count + 1))
      continue
    fi

    # Format to a temp file
    local tmp_file="$tmpdir/$(basename "$file")"
    ( cd "$REPO_ROOT" && "${SHADML[@]}" fmt "$file" ) > "$tmp_file"

    # Compile the formatted output
    if ( cd "$REPO_ROOT" && "${SHADML[@]}" compile "$tmp_file" ) &>/dev/null; then
      echo -e "  ${GREEN}pass${RESET}  $file"
      passed=$((passed + 1))
    else
      echo -e "  ${RED}FAIL${RESET}  $file"
      failed=$((failed + 1))
      failures+=("$file")
    fi
    count=$((count + 1))
  done

  echo ""
  echo -e "${BOLD}Results: $count total, $passed passed, $failed failed, $skipped skipped.${RESET}"

  if [[ $failed -gt 0 ]]; then
    echo ""
    echo -e "${RED}${BOLD}Failed files:${RESET}"
    for f in "${failures[@]}"; do
      echo "  $f"
    done
    exit 1
  fi
}

# ---------------------------------------------------------------------------
# Dispatch
# ---------------------------------------------------------------------------
case "${1:-}" in
  fmt)        mode_fmt ;;
  fmt-check)  mode_fmt_check ;;
  validate)   mode_validate ;;
  *)
    echo "Usage: $0 <fmt|fmt-check|validate>"
    echo ""
    echo "  fmt         Format all .shadml example files in-place"
    echo "  fmt-check   Check all files are already formatted (exit 1 if not)"
    echo "  validate    Format each file, then compile the result"
    exit 1
    ;;
esac
